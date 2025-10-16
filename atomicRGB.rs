
use rgb_lib::{
    wallet::{Wallet, WalletData, Online, DatabaseType},
    Error, BitcoinNetwork, AssetSchema, Assignment,
    keys::generate_keys,
    bitcoin::{
        hashes::{Hash, sha256},
        PublicKey, ScriptBuf, Address, Network as BdkNetwork,
        script::Builder,
        opcodes::all::*,
    },
};
use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use reqwest::blocking::Client;
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RgbLnInvoice {
    pub payment_hash: String,
    pub amount_asset: u64,
    pub asset_id: String,
    pub description: String,
    pub expiry: u64,
}

#[derive(Debug, Clone)]
pub struct RgbLnNodeClient {
    base_url: String,
    api_key: Option<String>,
    client: Client,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecodeInvoiceResponse {
    pub payment_hash: String,
    pub amt_msat: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayInvoiceResponse {
    pub status: PaymentStatus,
    pub payment_hash: String,
    pub payment_secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PaymentStatus {
    Succeeded,
    Failed,
    Pending,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentDetails {
    pub amt_msat: u64,
    pub asset_amount: u64,
    pub asset_id: String,
    pub payment_hash: String,
    pub inbound: bool,
    pub status: PaymentStatus,
    pub created_at: u64,
    pub updated_at: u64,
    pub payee_pubkey: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preimage: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetPaymentResponse {
    pub payment: PaymentDetails,
}

impl RgbLnNodeClient {
    pub fn new(base_url: String, api_key: Option<String>) -> Self {
        Self {
            base_url,
            api_key,
            client: Client::new(),
        }
    }

    pub fn decode_invoice(&self, invoice: &str) -> Result<DecodeInvoiceResponse, Error> {
        println!("Decoding RGB-LN invoice...");
        
        let url = format!("{}/decodelninvoice", self.base_url);
        let mut request = self.client.post(&url)
            .json(&json!({ "invoice": invoice }));
        
        if let Some(ref key) = self.api_key {
            request = request.header("Authorization", format!("Bearer {}", key));
        }
        
        let response = request
            .send()
            .map_err(|e| Error::Internal {
                details: format!("Failed to decode invoice: {}", e),
            })?;

        if !response.status().is_success() {
            let error_msg = response.text().unwrap_or_else(|_| "Unknown error".to_string());
            return Err(Error::Internal {
                details: format!("RLN decode error: {}", error_msg),
            });
        }

        response.json::<DecodeInvoiceResponse>()
            .map_err(|e| Error::Internal {
                details: format!("Failed to parse decode response: {}", e),
            })
    }

    pub fn pay_invoice(&self, invoice: &str) -> Result<PayInvoiceResponse, Error> {
        println!("Paying RGB-LN invoice...");
        
        let url = format!("{}/sendpayment", self.base_url);
        let mut request = self.client.post(&url)
            .json(&json!({ "invoice": invoice }));
        
        if let Some(ref key) = self.api_key {
            request = request.header("Authorization", format!("Bearer {}", key));
        }
        
        let response = request
            .send()
            .map_err(|e| Error::Internal {
                details: format!("Payment failed: {}", e),
            })?;

        if !response.status().is_success() {
            let error_msg = response.text().unwrap_or_else(|_| "Unknown error".to_string());
            return Err(Error::Internal {
                details: format!("RLN payment error: {}", error_msg),
            });
        }

        let result = response.json::<PayInvoiceResponse>()
            .map_err(|e| Error::Internal {
                details: format!("Failed to parse payment response: {}", e),
            })?;

        println!("PayInvoiceResponse: {:?}", result);
        
        if result.status == PaymentStatus::Pending {
            println!("WARNING: Payment succeeded but status is Pending");
        }

        Ok(result)
    }

    pub fn get_payment(&self, payment_hash: &str) -> Result<GetPaymentResponse, Error> {
        println!("Getting payment details for hash: {}...", payment_hash);
        
        let url = format!("{}/getpayment", self.base_url);
        let mut request = self.client.post(&url)
            .json(&json!({ "payment_hash": payment_hash }));
        
        if let Some(ref key) = self.api_key {
            request = request.header("Authorization", format!("Bearer {}", key));
        }
        
        let response = request
            .send()
            .map_err(|e| Error::Internal {
                details: format!("Failed to get payment: {}", e),
            })?;

        if !response.status().is_success() {
            let error_msg = response.text().unwrap_or_else(|_| "Unknown error".to_string());
            return Err(Error::Internal {
                details: format!("RLN getPayment error: {}", error_msg),
            });
        }

        let result = response.json::<GetPaymentResponse>()
            .map_err(|e| Error::Internal {
                details: format!("Failed to parse payment details: {}", e),
            })?;

        println!("GetPaymentResponse: {:?}", result);
        Ok(result)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HtlcStatus {
    Created,
    AwaitingFunding,
    Funded,
    PaymentInProgress,
    Claimed,
    Refunded,
    Expired,
}

#[derive(Debug, Clone)]
pub struct AtomicRgbHtlc {
    pub swap_id: String,
    pub payment_hash: [u8; 32],
    pub amount: u64,
    pub asset_id: String,
    pub lp_pubkey: PublicKey,
    pub user_pubkey: PublicKey,
    pub timelock_blocks: u32,
    pub status: HtlcStatus,
    
    pub htlc_script: ScriptBuf,
    pub htlc_address: String,
    
    pub recipient_id: Option<String>,
    pub batch_transfer_idx: Option<u32>,
    pub preimage: Option<[u8; 32]>,
}

impl AtomicRgbHtlc {
    pub fn new(
        payment_hash: [u8; 32],
        amount: u64,
        asset_id: String,
        lp_pubkey: PublicKey,
        user_pubkey: PublicKey,
        timelock_blocks: u32,
        network: BdkNetwork,
    ) -> Self {
        use sha256::Hash;
        let swap_id = Hash::hash(&payment_hash).to_string();
        
        let htlc_script = Self::create_htlc_script(
            &payment_hash,
            &lp_pubkey,
            &user_pubkey,
            timelock_blocks,
        );
        
        let htlc_address = Address::p2wsh(&htlc_script, network).to_string();
        
        Self {
            swap_id,
            payment_hash,
            amount,
            asset_id,
            lp_pubkey,
            user_pubkey,
            timelock_blocks,
            status: HtlcStatus::Created,
            htlc_script,
            htlc_address,
            recipient_id: None,
            batch_transfer_idx: None,
            preimage: None,
        }
    }

    fn create_htlc_script(
        payment_hash: &[u8; 32],
        lp_pubkey: &PublicKey,
        user_pubkey: &PublicKey,
        timelock_blocks: u32,
    ) -> ScriptBuf {
        Builder::new()
            .push_opcode(OP_IF)
                .push_opcode(OP_SHA256)
                .push_slice(payment_hash)
                .push_opcode(OP_EQUALVERIFY)
                .push_key(lp_pubkey)
                .push_opcode(OP_CHECKSIG)
            .push_opcode(OP_ELSE)
                .push_int(timelock_blocks as i64)
                .push_opcode(OP_CSV)
                .push_opcode(OP_DROP)
                .push_key(user_pubkey)
                .push_opcode(OP_CHECKSIG)
            .push_opcode(OP_ENDIF)
            .into_script()
    }

    pub fn verify_preimage(&self, preimage: &[u8; 32]) -> bool {
        let hash = sha256::Hash::hash(preimage);
        let hash_bytes: &[u8] = hash.as_ref();
        hash_bytes == self.payment_hash.as_slice()
    }
}

pub struct AtomicRgbLnLiquidityProvider {
    wallet: Wallet,
    active_swaps: HashMap<String, AtomicRgbHtlc>,
    lp_pubkey: PublicKey,
    proxy_url: String,
    bitcoin_network: BdkNetwork,
    rgb_ln_client: RgbLnNodeClient,
}

impl AtomicRgbLnLiquidityProvider {
    pub fn new(
        wallet_data: WalletData,
        lp_pubkey: PublicKey,
        proxy_url: String,
        bitcoin_network: BdkNetwork,
        rgb_ln_base_url: String,
        rgb_ln_api_key: Option<String>,
    ) -> Result<Self, Error> {
        let wallet = Wallet::new(wallet_data)?;
        let rgb_ln_client = RgbLnNodeClient::new(rgb_ln_base_url, rgb_ln_api_key);
        
        Ok(Self {
            wallet,
            active_swaps: HashMap::new(),
            lp_pubkey,
            proxy_url,
            bitcoin_network,
            rgb_ln_client,
        })
    }

    #[cfg(any(feature = "electrum", feature = "esplora"))]
    pub fn go_online(
        &mut self,
        skip_consistency_check: bool,
        electrum_url: Option<String>,
    ) -> Result<Online, Error> {
        let online = self.wallet.go_online(
            skip_consistency_check,
            electrum_url.unwrap_or_else(|| "ssl://electrum.blockstream.info:60002".to_string()),
        )?;
        
        Ok(online)
    }

    pub fn create_atomic_swap(
        &mut self,
        invoice: RgbLnInvoice,
        user_pubkey: PublicKey,
    ) -> Result<AtomicSwapOffer, Error> {
        if invoice.asset_id.is_empty() {
            return Err(Error::Internal {
                details: "Invalid asset ID".to_string(),
            });
        }

        let payment_hash = hex::decode(&invoice.payment_hash)
            .map_err(|e| Error::Internal {
                details: format!("Invalid payment hash: {}", e),
            })?;
        let payment_hash: [u8; 32] = payment_hash.try_into()
            .map_err(|_| Error::Internal {
                details: "Payment hash must be 32 bytes".to_string(),
            })?;

        let htlc = AtomicRgbHtlc::new(
            payment_hash,
            invoice.amount_asset,
            invoice.asset_id.clone(),
            self.lp_pubkey.clone(),
            user_pubkey,
            144,
            self.bitcoin_network,
        );

        
        let receive_data = self.wallet.script_receive(
            htlc.htlc_script.clone(),
            None,
            rgb_lib::Assignment::Fungible(htlc.amount),
            Some(86400),
            vec![self.proxy_url.clone()],
            1,
        )?;
        
        let recipient_id = receive_data.recipient_id;
        let rgb_invoice = receive_data.invoice;

        let mut htlc = htlc;
        htlc.recipient_id = Some(recipient_id.clone());
        htlc.status = HtlcStatus::AwaitingFunding;
        
        let swap_id = htlc.swap_id.clone();
        let htlc_address = htlc.htlc_address.clone();
        self.active_swaps.insert(swap_id.clone(), htlc);

        Ok(AtomicSwapOffer {
            swap_id,
            htlc_address,
            recipient_id,
            rgb_invoice,
            payment_hash: invoice.payment_hash,
            timelock_blocks: 144,
        })
    }

    pub fn check_htlc_funding(
        &mut self,
        online: Online,
        swap_id: &str,
    ) -> Result<HtlcFundingStatus, Error> {
        let htlc = self.active_swaps.get(swap_id)
            .ok_or_else(|| Error::Internal {
                details: "Swap not found".to_string(),
            })?;

        if htlc.status == HtlcStatus::Funded {
            return Ok(HtlcFundingStatus::Funded);
        }

        let recipient_id = htlc.recipient_id.clone()
            .ok_or_else(|| Error::Internal {
                details: "HTLC has no recipient ID".to_string(),
            })?;

        println!("   🔄 Refreshing wallet to check for incoming transfers...");
        let refresh_result = self.wallet.refresh(
            online.clone(),
            None,
            vec![],
            false,
        )?;

        println!("   📊 Refresh complete: {} transfers updated", refresh_result.len());

        let assets = self.wallet.list_assets(vec![])?;
        let total_assets = 
            assets.nia.as_ref().map(|v| v.len()).unwrap_or(0) +
            assets.cfa.as_ref().map(|v| v.len()).unwrap_or(0) +
            assets.uda.as_ref().map(|v| v.len()).unwrap_or(0);
        
        println!("   💎 Assets in wallet: {}", total_assets);
        if let Some(ref nia_assets) = assets.nia {
            for asset in nia_assets {
                let balance = self.wallet.get_asset_balance(asset.asset_id.clone())?;
                println!("      - NIA {}: {} units (settled: {}, future: {})", 
                         asset.ticker, asset.asset_id, balance.settled, balance.future);
            }
        }
        if let Some(ref cfa_assets) = assets.cfa {
            for asset in cfa_assets {
                let balance = self.wallet.get_asset_balance(asset.asset_id.clone())?;
                println!("      - CFA {}: {} units (settled: {}, future: {})", 
                         asset.name, asset.asset_id, balance.settled, balance.future);
            }
        }

        let unspents = self.wallet.list_unspents(Some(online.clone()), false, false)?;
        let total_utxos = unspents.len();
        let total_btc: u64 = unspents.iter().map(|u| u.utxo.btc_amount).sum();
        println!("   🔷 UTXOs in wallet: {} (total: {} sats)", total_utxos, total_btc);
        
        let colored_utxos: Vec<_> = unspents.iter()
            .filter(|u| !u.rgb_allocations.is_empty())
            .collect();
        
        if !colored_utxos.is_empty() {
            println!("      Colored UTXOs: {}", colored_utxos.len());
            for unspent in colored_utxos {
                println!("      • {}:{} - {} sats", 
                         &unspent.utxo.outpoint.txid[..8],
                         unspent.utxo.outpoint.vout,
                         unspent.utxo.btc_amount);
                for allocation in &unspent.rgb_allocations {
                    let status = if allocation.settled { "✅" } else { "⏳" };
                    let amount = match &allocation.assignment {
                        Assignment::Fungible(amt) => format!("{} units", amt),
                        Assignment::NonFungible => "NFT".to_string(),
                        _ => "?".to_string(),
                    };
                    println!("        └─ {} {} {}",
                             status,
                             allocation.asset_id.as_ref().unwrap_or(&"?".to_string()),
                             amount);
                }
            }
        }

        let asset_filter = if let Some(ref nia_assets) = assets.nia {
            if !nia_assets.is_empty() {
                let asset_id = nia_assets[0].asset_id.clone();
                println!("   🔍 Filtering transfers by NIA asset: {}", asset_id);
                Some(asset_id)
            } else {
                None
            }
        } else {
            None
        };
        
        let transfers = self.wallet.list_transfers(asset_filter)?;
        println!("   📋 Total transfers: {}", transfers.len());
        
        for transfer in transfers {
            if transfer.recipient_id == Some(recipient_id.clone()) {
                println!("   ✅ Found transfer to HTLC!");
                println!("      Status: {:?}", transfer.status);
                println!("      Recipient: {}", transfer.recipient_id.as_ref().unwrap());
                
                use rgb_lib::TransferStatus;
                if transfer.status == TransferStatus::Settled {
                    return Ok(HtlcFundingStatus::Funded);
                } else {
                    return Ok(HtlcFundingStatus::Pending);
                }
            }
        }
        
        Ok(HtlcFundingStatus::Pending)
    }

    pub fn pay_invoice(
        &mut self,
        swap_id: &str,
        invoice_string: &str,
    ) -> Result<PaymentResult, Error> {
        let htlc = self.active_swaps.get_mut(swap_id)
            .ok_or_else(|| Error::Internal {
                details: "Swap not found".to_string(),
            })?;

        if htlc.status != HtlcStatus::Funded {
            return Err(Error::Internal {
                details: "HTLC not funded yet".to_string(),
            });
        }

        htlc.status = HtlcStatus::PaymentInProgress;

        let decode_response = self.rgb_ln_client.decode_invoice(invoice_string)?;
        
        if decode_response.payment_hash != hex::encode(htlc.payment_hash) {
            return Err(Error::Internal {
                details: "Payment hash mismatch between invoice and HTLC".to_string(),
            });
        }

        let pay_response = self.rgb_ln_client.pay_invoice(invoice_string)?;
        
        let payment_details = self.rgb_ln_client.get_payment(&pay_response.payment_hash)?;
        
        match payment_details.payment.status {
            PaymentStatus::Succeeded => {
                if let Some(preimage_hex) = payment_details.payment.preimage {
                    Ok(PaymentResult {
                        success: true,
                        preimage: Some(preimage_hex),
                        error: None,
                    })
                } else {
                    Err(Error::Internal {
                        details: "Payment succeeded but no preimage returned".to_string(),
                    })
                }
            },
            PaymentStatus::Pending => {
                Ok(PaymentResult {
                    success: false,
                    preimage: None,
                    error: Some("Payment is pending".to_string()),
                })
            },
            PaymentStatus::Failed => {
                Err(Error::Internal {
                    details: "Payment failed".to_string(),
                })
            }
        }
    }

    pub fn claim_htlc_atomic(
        &mut self,
        swap_id: &str,
        preimage: [u8; 32],
    ) -> Result<AtomicClaimResult, Error> {
        let htlc = self.active_swaps.get_mut(swap_id)
            .ok_or_else(|| Error::Internal {
                details: "Swap not found".to_string(),
            })?;

        if !htlc.verify_preimage(&preimage) {
            return Err(Error::Internal {
                details: "Invalid preimage - hash doesn't match!".to_string(),
            });
        }

        
        htlc.status = HtlcStatus::Claimed;
        htlc.preimage = Some(preimage);

        Ok(AtomicClaimResult {
            swap_id: swap_id.to_string(),
            amount_claimed: htlc.amount,
            asset_id: htlc.asset_id.clone(),
            preimage_hex: hex::encode(preimage),
            claim_txid: "placeholder_txid".to_string(),
        })
    }

    pub fn get_refund_info(&self, swap_id: &str) -> Result<RefundInfo, Error> {
        let htlc = self.active_swaps.get(swap_id)
            .ok_or_else(|| Error::Internal {
                details: "Swap not found".to_string(),
            })?;

        Ok(RefundInfo {
            swap_id: swap_id.to_string(),
            htlc_address: htlc.htlc_address.clone(),
            htlc_script: htlc.htlc_script.clone(),
            timelock_blocks: htlc.timelock_blocks,
            can_refund: htlc.status != HtlcStatus::Claimed,
        })
    }

    pub fn complete_atomic_swap(
        &mut self,
        swap_id: &str,
        invoice_string: &str,
    ) -> Result<AtomicClaimResult, Error> {
        let payment_result = self.pay_invoice(swap_id, invoice_string)?;
        
        if !payment_result.success {
            return Err(Error::Internal {
                details: format!("Payment failed: {:?}", payment_result.error),
            });
        }

        let preimage_hex = payment_result.preimage
            .ok_or_else(|| Error::Internal {
                details: "No preimage in payment result".to_string(),
            })?;

        let preimage_bytes = hex::decode(&preimage_hex)
            .map_err(|e| Error::Internal {
                details: format!("Invalid preimage hex: {}", e),
            })?;
        
        let preimage: [u8; 32] = preimage_bytes.try_into()
            .map_err(|_| Error::Internal {
                details: "Preimage must be 32 bytes".to_string(),
            })?;

        self.claim_htlc_atomic(swap_id, preimage)
    }
}


#[derive(Debug, Serialize, Deserialize)]
pub struct AtomicSwapOffer {
    pub swap_id: String,
    pub htlc_address: String,
    pub recipient_id: String,
    pub rgb_invoice: String,
    pub payment_hash: String,
    pub timelock_blocks: u32,
}

#[derive(Debug, PartialEq)]
pub enum HtlcFundingStatus {
    Pending,
    Funded,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PaymentResult {
    pub success: bool,
    pub preimage: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AtomicClaimResult {
    pub swap_id: String,
    pub amount_claimed: u64,
    pub asset_id: String,
    pub preimage_hex: String,
    pub claim_txid: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RefundInfo {
    pub swap_id: String,
    pub htlc_address: String,
    pub htlc_script: ScriptBuf,
    pub timelock_blocks: u32,
    pub can_refund: bool,
}

fn main() -> Result<(), Error> {
    println!("Demo");

    let data_dir = std::env::temp_dir().join("atomic_swap_demo");
    if !data_dir.exists() {
        std::fs::create_dir_all(&data_dir)
            .map_err(|e| Error::Internal { details: format!("Failed to create dir: {}", e) })?;
    }
    let lp_keys = generate_keys(BitcoinNetwork::Regtest);
    let wallet_data = WalletData {
        data_dir: data_dir.to_string_lossy().to_string(),
        bitcoin_network: BitcoinNetwork::Regtest,
        database_type: DatabaseType::Sqlite,
        max_allocations_per_utxo: 1,
        account_xpub_vanilla: lp_keys.account_xpub_vanilla.clone(),
        account_xpub_colored: lp_keys.account_xpub_colored.clone(),
        mnemonic: Some(lp_keys.mnemonic.clone()),
        master_fingerprint: lp_keys.master_fingerprint.clone(),
        vanilla_keychain: Some(1),
        supported_schemas: vec![
            AssetSchema::Nia,
        ],
    };

    let _wallet = Wallet::new(wallet_data.clone())?;
    println!("LP wallet created successfully!\n");

    use std::str::FromStr;
    use rgb_lib::bitcoin::bip32::Xpub;
    
    let xpub = Xpub::from_str(&lp_keys.account_xpub_colored)
        .expect("Valid xPub");
    
    let secp = rgb_lib::bitcoin::secp256k1::Secp256k1::new();
    let derived_xpub = xpub.derive_pub(&secp, &[
        rgb_lib::bitcoin::bip32::ChildNumber::from_normal_idx(0).unwrap()
    ]).expect("Derivation succeeds");
    
    let lp_pubkey = PublicKey::new(derived_xpub.public_key);
    
    println!("LP Public Key (from wallet): {}\n", lp_pubkey);

    let user_pubkey = PublicKey::from_str(
        "03d6c27614557184d269b9cb19b1bc32479e661d86a925f4c4e46c734adcea3d19"
    ).expect("Valid user pubkey");
    println!("User Public Key: {}\n", user_pubkey);

    
    let preimage_hex = "86a85cd1cb86c51186d190972c9f8413f436911fc0de241b6df20877ebbadecc";
    let payment_hash_hex = "f4d376425855e2354bf30e17904f4624f6f9aa297973cca0445cdf4cef718b2a";
    
    let preimage_bytes = hex::decode(preimage_hex)
        .expect("Valid preimage hex");
    let preimage: [u8; 32] = preimage_bytes.try_into()
        .expect("Preimage is 32 bytes");
    
    let payment_hash_bytes = hex::decode(payment_hash_hex)
        .expect("Valid payment hash hex");
    let payment_hash: [u8; 32] = payment_hash_bytes.try_into()
        .expect("Payment hash is 32 bytes");
    
    let computed_hash = sha256::Hash::hash(&preimage);
    let computed_hash_bytes: &[u8] = computed_hash.as_ref();
    
    println!(" Payment Data");
    println!("   Preimage:     {}", preimage_hex);
    println!("   Payment Hash: {}", payment_hash_hex);
    println!("   Verified:     {}\n", computed_hash_bytes == &payment_hash[..]);

    let invoice = RgbLnInvoice {
        payment_hash: payment_hash_hex.to_string(),
        amount_asset: 13,
        asset_id: "rgb:AxBwL0~H-EAIs51Q-p1rNBjG-NYkBmNb-gt~mV4o-bFC7GPg".to_string(),
        description: "Test RGB-LN Payment".to_string(),
        expiry: 36000,
    };

    println!("RGB-LN Invoice:");
    println!("   Payment Hash: {}", invoice.payment_hash);
    println!("   Amount: {} asset units", invoice.amount_asset);
    println!("   Asset ID: {}", invoice.asset_id);
    println!("   Description: {}\n", invoice.description);

    println!("Initializing Atomic LP Service...");
    let mut lp = AtomicRgbLnLiquidityProvider::new(
        wallet_data,
        lp_pubkey,
        "rpc://regtest.thunderstack.org:3000/json-rpc".to_string(),
        BdkNetwork::Regtest,
        "http://localhost:3000".to_string(),
        None,
    )?;
    println!(" LP ready!\n");

    println!("Creating ATOMIC HTLC swap...");
    let offer = lp.create_atomic_swap(invoice.clone(), user_pubkey)?;
    
    println!(" HTLC Created!");
    println!("   Swap ID: {}", offer.swap_id);
    println!("   HTLC Address: {}", offer.htlc_address);
    println!("   Recipient ID: {}", offer.recipient_id);
    println!("   Payment Hash: {}", offer.payment_hash);
    println!("   Timelock: {} blocks\n", offer.timelock_blocks);

    println!("RGB Invoice for User:");
    println!("   {}\n", offer.rgb_invoice);
    println!("   User should send {} units of {} to this address", 
             invoice.amount_asset, invoice.asset_id);

    println!("HTLC Script Guarantees:");
    println!("   IF (preimage SHA256 == {}):", hex::encode(&payment_hash[..8]));
    println!("      LP can claim with signature");
    println!("   ELSE:");
    println!("     User can refund after {} blocks\n", offer.timelock_blocks);



  
    
    #[cfg(any(feature = "electrum", feature = "esplora"))]
    {
        println!("\n  DEMO");
        println!("===========================================\n");
        
        match lp.go_online(false, Some("tcp://regtest.thunderstack.org:50001".to_string())) {
            Ok(online) => {
                println!("Wallet ONLINE!");
                use std::time::{Duration, Instant};
                use std::thread;
                
                let start_time = Instant::now();
                let timeout = Duration::from_secs(1200);
                let mut check_count = 0;
                
                loop {
                    check_count += 1;
                    let elapsed = start_time.elapsed();
                    
                    match lp.check_htlc_funding(online.clone(), &offer.swap_id) {
                        Ok(status) => {
                            match status {
                                HtlcFundingStatus::Funded => {
                                    println!("SUCCESS! HTLC is FUNDED!");
                                    break;
                                }
                                HtlcFundingStatus::Pending => {
                                    println!("Status: Pending (WaitingCounterparty)");
                                    
                                    thread::sleep(Duration::from_secs(30));
                                }
                            }
                        }
                        Err(e) => {
                            println!("Error: {}", e);
                            thread::sleep(Duration::from_secs(30));
                            
                            if elapsed > timeout {
                                break;
                            }
                        }
                    }
                }
                
            }
            Err(e) => {
            }
        }
        
       
    }
    Ok(())
}

