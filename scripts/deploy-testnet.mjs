// Deploys a fresh copy of The Heist + tGOLD on NEAR testnet, funds the vault and plays one smoke-test heist.
// Keys are generated here and never stored: once this run ends, nobody holds a key to these contracts.
// Usage: node deploy-testnet.mjs <dir with the_heist.wasm and test_gold.wasm> <output deployment.json>
import { connect, keyStores, KeyPair, utils } from 'near-api-js';
import { readFile, writeFile } from 'node:fs/promises';

const [wasmDir, outFile] = process.argv.slice(2);
const RPC = process.env.NEAR_RPC || 'https://test.rpc.fastnear.com';
const ONE = 10n ** 18n;
const BURN = '0'.repeat(64); // implicit account of an all-zero key: nobody can ever sign for it
const stamp = Date.now().toString(36);
const heistId = `robinhoot-heist-${stamp}.testnet`;
const goldId = `tgold.${heistId}`;

const keyStore = new keyStores.InMemoryKeyStore();
const key = KeyPair.fromRandom('ed25519');
await keyStore.setKey('testnet', heistId, key);
await keyStore.setKey('testnet', goldId, key);
const near = await connect({ networkId: 'testnet', nodeUrl: RPC, keyStore });

async function createWithFaucet(accountId) {
  const body = JSON.stringify({ newAccountId: accountId, newAccountPublicKey: key.getPublicKey().toString() });
  for (const url of ['https://helper.nearprotocol.com/account', 'https://helper.testnet.near.org/account']) {
    try {
      const r = await fetch(url, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body });
      console.log(`faucet ${url}: ${r.status} ${(await r.text()).slice(0, 200)}`);
      if (r.ok) return;
    } catch (e) { console.log(`faucet ${url}: ${e.message}`); }
  }
  throw new Error('No testnet faucet answered');
}
const wait = ms => new Promise(r => setTimeout(r, ms));
const call = (account, contractId, methodName, args, deposit = '0', gas = 100n * 10n ** 12n) =>
  account.functionCall({ contractId, methodName, args, gas, attachedDeposit: BigInt(deposit) });

console.log('Creating', heistId);
await createWithFaucet(heistId);
await wait(4000);
const heist = await near.account(heistId);
console.log('balance', utils.format.formatNearAmount((await heist.getAccountBalance()).available));

console.log('Creating', goldId);
await heist.createAccount(goldId, key.getPublicKey(), BigInt(utils.format.parseNearAmount('3')));
const gold = await near.account(goldId);

console.log('Deploying tGOLD');
await gold.deployContract(await readFile(`${wasmDir}/test_gold.wasm`));
await call(gold, goldId, 'new', { owner: goldId, supply: (1_000_000_000n * ONE).toString() });

console.log('Deploying The Heist');
await heist.deployContract(await readFile(`${wasmDir}/the_heist.wasm`));
await call(heist, heistId, 'new', { owner: heistId, token: goldId, burn_account: BURN, edge_bps: 300, max_win_bps: 100, min_bet: ONE.toString() });

console.log('Registering the vault and the burn address with tGOLD');
await call(gold, goldId, 'faucet', { account: heistId });
await call(gold, goldId, 'faucet', { account: BURN });

console.log('Funding the vault with 10,000,000 tGOLD');
await call(gold, goldId, 'ft_transfer_call', { receiver_id: heistId, amount: (10_000_000n * ONE).toString(), msg: '"fund"' }, '1', 150n * 10n ** 12n);

console.log('Smoke test: one heist at 2.00x for 100 tGOLD');
await call(gold, goldId, 'ft_transfer_call', { receiver_id: heistId, amount: (100n * ONE).toString(), msg: JSON.stringify({ heist: { escape: 200 } }) }, '1', 150n * 10n ** 12n);
await wait(3000);
const view = (id, m, a = {}) => heist.viewFunction({ contractId: id, methodName: m, args: a });
console.log('recent', JSON.stringify(await view(heistId, 'get_recent', { limit: 1 })));
console.log('config', JSON.stringify(await view(heistId, 'get_config')));
console.log('burned so far', await view(goldId, 'ft_balance_of', { account_id: BURN }));

// Lock it: delete the only full-access keys. From now on the code rules alone.
for (const account of [gold, heist]) await account.deleteKey(key.getPublicKey());
console.log('Keys deleted: the contracts are locked.');

await writeFile(outFile, JSON.stringify({ network: 'testnet', heist: heistId, token: goldId, burn: BURN, rpc: RPC, deployedAt: new Date().toISOString() }, null, 2) + '\n');
console.log('Wrote', outFile);
