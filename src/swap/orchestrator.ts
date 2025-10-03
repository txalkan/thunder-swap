import { rlnClient } from '../rln/client.js';
import { rpc } from '../bitcoin/rpc.js';
import { waitForFunding } from '../bitcoin/watch.js';
import { buildHtlcRedeemScript } from '../bitcoin/htlc.js';
import { claimWithPreimage } from '../bitcoin/claim.js';
import { buildRefundPsbtBase64 } from '../bitcoin/refund.js';
import { sha256hex, hexToBuffer } from '../utils/crypto.js';
import BIP32Factory from 'bip32';
import * as tools from 'uint8array-tools';
import * as ecc from 'tiny-secp256k1';
import { BIP32Interface } from 'bip32';

import { config } from '../config.js';
import * as bitcoin from 'bitcoinjs-lib';
import { Signer, SignerAsync, ECPairInterface, ECPairFactory, ECPairAPI, TinySecp256k1Interface } from 'ecpair';
import * as tinysecp from 'tiny-secp256k1';
const ECPair: ECPairAPI = ECPairFactory(tinysecp);



const network = bitcoin.networks.testnet;

interface SwapParams {
  invoice: string;
  userRefundPubkeyHex: string;
  userRefundAddress: string;
}

interface SwapResult {
  success: boolean;
  txid?: string;
  psbt?: string;
  instructions?: string;
  error?: string;
}

/**
 * Main swap orchestrator that handles the complete submarine swap flow
 */
export async function runSwap({
  invoice,
  userRefundPubkeyHex,
  userRefundAddress
}: SwapParams): Promise<SwapResult> {
  try {
    console.log('Starting RGB-LN submarine swap...\n');

    // Step 1: Decode RGB-LN invoice to get payment hash and amount
    console.log('Step 1: Decoding RGB-LN invoice...');
    const decodedInvoice = await rlnClient.decode(invoice);
    const H = decodedInvoice.payment_hash;
    const amount_sat = decodedInvoice.amt_msat;
    const expires_at = decodedInvoice.expires_at;

    console.log(`   Payment Hash (H): ${H}`);
    console.log(`   Amount: ${amount_sat} sats`);
    if (expires_at) {
      console.log(`   Expires: ${new Date(expires_at * 1000).toISOString()}`);
    }

    // Validate H format
    if (!H.match(/^[0-9a-fA-F]{64}$/)) {
      throw new Error('Invalid payment hash format from RGB-LN invoice');
    }

    // Step 2: Read tip height and set timeout block height
    console.log('\nStep 2: Setting timelock...');
    const tipHeight = await rpc.getBlockCount();
    const tLock = tipHeight + config.LOCKTIME_BLOCKS;
    console.log(`   Current block height: ${tipHeight}`);
    console.log(`   Time lock block height: ${tLock}`);

    // Security check: ensure invoice hasn't expired and we have enough time
    if (expires_at && expires_at <= Date.now() / 1000) {
      throw new Error('Invoice has expired');
    }

    // Step 3: Generate LP pubkey from WIF
    // console.log('\nStep 3: Preparing LP keys...');
    // let lpKeyPair: ECPairInterface;
    // try {
    //   lpKeyPair = ECPair.fromWIF(config.LP_WIF);
    // } catch (error) {
    //   throw new Error(`Invalid LP WIF: ${error}`);
    // }
    // const lpPubkeyHex = tools.toHex(lpKeyPair.publicKey);
    // console.log(`   LP Public Key: ${lpPubkeyHex}`);  
    // const network = bitcoin.networks.testnet;

    // 1) Your account xprv/tprv from listdescriptors(true)
   
    
    // 2) Derive to the exact path (example: m/84'/1'/0'/0/1 for your addr)
    const CHAIN = 0;   // 0 = external, 1 = change
    const INDEX = 1;
    const bip32 = BIP32Factory(ecc);

    const node = bip32.fromBase58(config.LP_ACCOUNT_XPRV, network);
    // const child = node.derive(CHAIN).derive(INDEX);

    const child = node.derivePath(`m/84'/1'/0'/0/${INDEX}`);
    console.log('node.derive(CHAIN).derive(INDEX);',tools.toHex(child.publicKey))
    if (!child.privateKey) throw new Error("No private key (did you use tpub instead of tprv?)");
    
    // 3) Build ECPair from the raw 32-byte private key
    console.log(network)
    const keyPair2 = ECPair.fromPrivateKey(child.privateKey, { network, compressed: true });
    
    // Optional: WIF / address check
    const wif = keyPair2.toWIF();
    // Step 3: Generate LP pubkey from WIF
    let lpKeyPair: ECPairInterface;
    try {
      lpKeyPair = ECPair.fromWIF(wif,network);
    } catch (error) {
      throw new Error(`Invalid LP WIF: ${error}`);
    }
    const lpPubkeyHex = tools.toHex(lpKeyPair.publicKey);
    console.log(`   LP Public Key: ${lpPubkeyHex}`);  
    // const addr = bitcoin.payments.p2wpkh({ pubkey: keyPair2.publicKey, network }).address;
    // console.log({ wif, addr });

    // Step 4: Build HTLC redeem script and P2WSH address
    console.log('\nStep 4: Building HTLC...');
    const htlcResult = buildHtlcRedeemScript(H, lpPubkeyHex, userRefundPubkeyHex, tLock);
    console.log(`   P2WSH HTLC Address: ${htlcResult.p2wshAddress}`);
    console.log(`   Amount to fund: ${amount_sat} sats`);
    console.log(`   Redeem Script Hash: ${sha256hex(htlcResult.redeemScript)}`);

    
    // Step 5: Wait for funding transaction confirmation
    console.log('\nStep 5: Waiting for funding confirmation...');
    const funding = await waitForFunding(htlcResult.p2wshAddress, config.MIN_CONFS);
    console.log(`   Funding confirmed: ${funding.txid}:${funding.vout} (${funding.value} sats)`);

    // Step 6: Pay RGB-LN invoice
    console.log('\nStep 6: Paying RGB-LN invoice...');
    const paymentResult = await rlnClient.pay(invoice);

    if (paymentResult.status === 'succeeded') {
      let preimage = paymentResult.preimage;
      
      // If preimage not returned in payment response, try to get it via getPayment API
      if (!preimage) {
        console.log('\nStep 6b: Payment succeeded but no preimage returned, fetching via getPayment...');
        try {
          const paymentDetails = await rlnClient.getPayment(H);
          preimage = paymentDetails.payment.preimage;
          
          if (!preimage) {
            console.error('Fatal: Payment succeeded but no preimage available via getPayment API!');
            console.error('   Check your RGB-LN implementation to include preimage in payment responses.');
            return { success: false, error: 'Payment succeeded but no preimage available' };
          }
          console.log(`   Preimage retrieved via getPayment: ${preimage}`);
        } catch (error: any) {
          console.error('Fatal: Failed to get preimage via getPayment API!');
          console.error(`   Error: ${error.message}`);
          return { success: false, error: `Failed to get preimage: ${error.message}` };
        }
      }

      // const preimage = "1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";

      // Step 7: Verify preimage matches hash
      console.log('\nStep 7: Verifying preimage...');
      const preimageHash = sha256hex(hexToBuffer(preimage));
      if (preimageHash !== H) {
        throw new Error(`Preimage verification failed: ${preimageHash} !== ${H}`);
      }
      console.log(`   Preimage verified: ${preimage}`);

      // Step 8: Claim HTLC with preimage
      console.log('\nStep 8: Claiming HTLC...');
      const claimResult = await claimWithPreimage(
        { txid: funding.txid, vout: funding.vout, value: funding.value },
        htlcResult.redeemScript,
        preimage,
        config.LP_WIF,
        config.LP_CLAIM_ADDRESS
      );

      console.log(`   Claim transaction broadcast: ${claimResult.txid}`);
      return { success: true, txid: claimResult.txid };

    } else {
      // Step 8b: Payment failed - prepare refund PSBT
      console.log('\nStep 8b: Payment failed, preparing refund PSBT...');
      const refundResult = await buildRefundPsbtBase64(
        { txid: funding.txid, vout: funding.vout, value: funding.value },
        htlcResult.redeemScript,
        userRefundAddress,
        tLock
      );

      console.log(`   Refund PSBT prepared (base64): ${refundResult.psbtBase64}`);
      console.log('   Instructions:');
      console.log(refundResult.instructions);

      return {
        success: false,
        psbt: refundResult.psbtBase64,
        instructions: refundResult.instructions
      };
    }

  } catch (error: any) {
    const errorMsg = error.message || String(error);
    console.error(`Swap failed: ${errorMsg}`);
    console.error('\nTroubleshooting:');
    console.error('   - Check NETWORK setting matches your Bitcoin node');
    console.error('   - Verify RLN node is running and API accessible');
    console.error('   - Ensure funding transaction was sent to the correct HTLC address');
    console.error('   - Check LP_WIF and LP_CLAIM_ADDRESS in environment');
    return { success: false, error: errorMsg };
  }
}
