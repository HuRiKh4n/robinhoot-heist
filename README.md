# The Heist 🦊🏹

**A crash game on NEAR where every failed heist burns the memecoin.**
Built for NEAR HACKATHON 2026 (tracks: *Games With Real Economies*, *Memecoins With Mechanics*).

▶ **Play:** https://hurikh4n.github.io/robinhoot-heist/ (practice mode needs nothing; on-chain mode uses NEAR testnet and free tGOLD)

In the tale of [RobinHoot](https://robinhoot.fun), a whale Sheriff hoards the kingdom's gold and a guild of outlaws steals it back.
The Heist is the guild's game: you sneak into the Sheriff's vault, the loot multiplier climbs, and you decide **before you go in** when you will run.

## How it plays

1. **Pick your bet and your escape**, from 1.01× to 1000×.
2. **NEAR rolls the dice.** The contract turns the block's random seed (mixed with your account and the heist number) into a *crash point*: how far the heist gets before the Sheriff catches the thief.
3. **Escape or get caught**, settled in the same transaction:
   - crash ≥ escape → you are paid **bet × escape** at once;
   - crash < escape → **half of the bet is burned for good**, half stays in the vault that pays the next winners.

## The mechanic (why it is good for the coin)

| | |
|---|---|
| Win chance at escape *x* | **(1 − 3%) ÷ x** → 48.5% at 2×, 9.7% at 10×, 0.97% at 100× |
| House edge | 3%, fixed at deploy time, so the vault cannot run dry over time |
| Biggest possible win | 1% of the vault per heist, so one lucky roll cannot drain it |
| Every lost heist | 50% **burned** to an address nobody controls · 50% back to the vault |
| Admin keys | **none**: the deploy script deletes the only full-access keys after setup |

Playing makes the coin scarcer: the burn is proportional to the volume the game sees, and the burned balance is public.

## What is in this repo

```
contracts/heist       The Heist (Rust, near-sdk 5): ft_on_transfer game, crash roll, payouts, burn, vault, views, events
contracts/test-gold   tGOLD, a NEP-141 test coin with a public faucet, so anyone can play on testnet
scripts/              deploy-testnet.mjs: fresh accounts, deploy, fund the vault, one smoke-test heist, delete keys
web/                  the game page (plain HTML/CSS/JS, no build step): practice mode, wallet, live heists from the chain
.github/workflows     contracts.yml (tests + wasm build), deploy-testnet.yml, pages.yml
```

### Contract interface

Play by sending the coin to the contract with `ft_transfer_call`:

```jsonc
// msg to play a heist, running away at 2.50x
{"heist": {"escape": 250}}
// msg to top up the vault (anyone can)
"fund"
```

Views: `get_config`, `get_stats`, `get_recent({limit})`, `get_player({account})`, `max_bet({escape})`, `win_chance_bps({escape})`, `get_owed({account})`.
Each heist emits an `EVENT_JSON` log (`standard: "heist"`) with the bet, escape, crash, result, payout and burn.
A bet that cannot be played (vault paused, escape out of range, below the minimum, or a possible win above the cap) is refunded whole by the token in the same transaction.

### The crash formula

With `r` the first 8 bytes of `sha256(random_seed ‖ player ‖ heist_id)` as an integer in `[0, 2^64)`:

```
crash = (1 − edge) / (1 − r / 2^64), floored to 0.01×, at least 1.00×
⇒ P(crash ≥ x) = (1 − edge) / x
```

The unit test `crash_math_matches_the_odds` checks the distribution over 100,000 evenly spread values.

### Live on testnet

The current deployment is in [`web/deployment.json`](web/deployment.json). The contract and coin accounts have no access keys left: check them on [testnet.nearblocks.io](https://testnet.nearblocks.io).

## Run it yourself

```bash
# contracts: unit tests and wasm (needs Rust + cargo-near)
cargo test --workspace
for c in heist test-gold; do (cd contracts/$c && cargo near build non-reproducible-wasm --no-abi); done
mkdir -p out && cp target/near/the_heist/the_heist.wasm target/near/test_gold/test_gold.wasm out/

# a fresh testnet deployment (creates accounts with the testnet faucet)
cd scripts && npm install && node deploy-testnet.mjs ../out ../web/deployment.json

# the page
cd web && python3 -m http.server 8000
```

The deploy workflow (`Actions → Deploy to testnet`) does the same on GitHub's machines and commits the new addresses.

## Fairness, honestly

NEAR's `random_seed` is unknown to players when they send a heist, and a player contract cannot undo a lost heist (the result is settled in the game's own receipt). Block producers could in theory bias a seed, which is why a single heist can never win more than 1% of the vault. For mainnet the same contract runs against the real memecoin with a vault funded from the project's public marketing wallet.

## Built at NEAR HACKATHON

All code in this repository was written during NEAR HACKATHON (1–14 November 2026). The art comes from the RobinHoot project.
