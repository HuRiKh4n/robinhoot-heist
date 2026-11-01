//! The Heist: a crash game on NEAR.
//!
//! An outlaw sends memecoins to this contract with `ft_transfer_call`, choosing the multiplier
//! they will run away at (the "escape"). The contract rolls how far the heist goes before the
//! Sheriff catches the thief (the "crash"), using NEAR's on-chain randomness:
//!
//! * crash >= escape: the outlaw escapes and is paid `bet * escape` at once.
//! * crash <  escape: the heist fails. Half of the bet is burned, half stays in the vault
//!   that pays the next winners.
//!
//! The house edge is fixed at deploy time (3% by default): P(crash >= x) = (1 - edge) / x.
//! A single heist can never win more than a small share of the vault, so the vault cannot be
//! drained by one lucky roll.

use near_sdk::borsh::{BorshDeserialize, BorshSerialize};
use near_sdk::json_types::U128;
use near_sdk::serde::{Deserialize, Serialize};
use near_sdk::store::{LookupMap, Vector};
use near_sdk::{
    env, ext_contract, log, near, require, AccountId, BorshStorageKey, Gas, NearToken, PanicOnDefault,
    PromiseOrValue, PromiseResult,
};

/// Multipliers are stored ×100: 250 means 2.50x.
pub const X: u128 = 100;
pub const MIN_ESCAPE: u32 = 101; // 1.01x
pub const MAX_ESCAPE: u32 = 100_000; // 1000x
pub const MAX_CRASH: u32 = 1_000_000; // 10000x, a ceiling for display only
const RECENT: u32 = 50;
const GAS_FT_TRANSFER: Gas = Gas::from_tgas(10);
const GAS_CALLBACK: Gas = Gas::from_tgas(10);
const ONE_YOCTO: NearToken = NearToken::from_yoctonear(1);

#[derive(BorshSerialize, BorshStorageKey)]
#[borsh(crate = "near_sdk::borsh")]
enum Key {
    Recent,
    Owed,
    Players,
}

#[derive(BorshSerialize, BorshDeserialize, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[borsh(crate = "near_sdk::borsh")]
#[serde(crate = "near_sdk::serde")]
pub struct Heist {
    pub id: u64,
    pub player: AccountId,
    pub bet: U128,
    /// Multiplier the outlaw chose to run away at (×100).
    pub escape: u32,
    /// How far the heist went before the Sheriff caught the thief (×100).
    pub crash: u32,
    pub won: bool,
    /// Paid to the player (bet × escape) when won, 0 otherwise.
    pub payout: U128,
    /// Burned when lost (half of the bet), 0 otherwise.
    pub burned: U128,
    pub block: u64,
    pub timestamp_ms: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, BorshSerialize, BorshDeserialize)]
#[serde(crate = "near_sdk::serde")]
#[borsh(crate = "near_sdk::borsh")]
pub struct Stats {
    pub heists: u64,
    pub escapes: u64,
    pub wagered: U128,
    pub paid: U128,
    pub burned: U128,
}

impl Default for Stats {
    fn default() -> Self {
        Self { heists: 0, escapes: 0, wagered: U128(0), paid: U128(0), burned: U128(0) }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(crate = "near_sdk::serde")]
pub struct Config {
    pub owner: AccountId,
    pub token: AccountId,
    pub burn_account: AccountId,
    pub edge_bps: u16,
    pub max_win_bps: u16,
    pub min_bet: U128,
    pub vault: U128,
    pub max_bet_at_2x: U128,
    pub paused: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(crate = "near_sdk::serde")]
pub struct PlayerStats {
    pub heists: u64,
    pub escapes: u64,
    pub wagered: U128,
    pub won: U128,
    pub best_escape: u32,
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Default)]
#[borsh(crate = "near_sdk::borsh")]
struct PlayerRecord {
    heists: u64,
    escapes: u64,
    wagered: u128,
    won: u128,
    best_escape: u32,
}

/// What the outlaw writes in the `msg` of `ft_transfer_call`.
#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde")]
#[serde(rename_all = "snake_case")]
enum Message {
    /// Play a heist, running away at `escape` (×100, e.g. 200 = 2.00x).
    Heist { escape: u32 },
    /// Add coins to the vault (anyone can top it up; nobody gets anything back).
    Fund,
}

#[ext_contract(ext_ft)]
#[allow(dead_code)]
trait FungibleToken {
    fn ft_transfer(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>);
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    owner: AccountId,
    token: AccountId,
    burn_account: AccountId,
    edge_bps: u16,
    max_win_bps: u16,
    min_bet: u128,
    vault: u128,
    paused: bool,
    stats: Stats,
    recent: Vector<Heist>,
    next_id: u64,
    /// Payouts or burns whose transfer failed, kept safe until retried.
    owed: LookupMap<AccountId, u128>,
    players: LookupMap<AccountId, PlayerRecord>,
}

#[near]
impl Contract {
    /// `token`: the memecoin contract. `burn_account`: an account with no keys where burned coins
    /// are sent (anyone can check it can never move them). `edge_bps`: house edge (300 = 3%).
    /// `max_win_bps`: the most a single heist can win, as a share of the vault (100 = 1%).
    #[init]
    pub fn new(
        owner: AccountId,
        token: AccountId,
        burn_account: AccountId,
        edge_bps: Option<u16>,
        max_win_bps: Option<u16>,
        min_bet: U128,
    ) -> Self {
        let edge_bps = edge_bps.unwrap_or(300);
        let max_win_bps = max_win_bps.unwrap_or(100);
        require!(edge_bps >= 100 && edge_bps <= 1000, "Edge must be between 1% and 10%");
        require!(max_win_bps >= 10 && max_win_bps <= 1000, "Max win must be between 0.1% and 10% of the vault");
        Self {
            owner,
            token,
            burn_account,
            edge_bps,
            max_win_bps,
            min_bet: min_bet.0,
            vault: 0,
            paused: false,
            stats: Stats::default(),
            recent: Vector::new(Key::Recent),
            next_id: 0,
            owed: LookupMap::new(Key::Owed),
            players: LookupMap::new(Key::Players),
        }
    }

    /// NEP-141 receiver: every heist and every vault top-up arrives here.
    /// Returns the amount to refund (the whole bet when it cannot be played).
    pub fn ft_on_transfer(&mut self, sender_id: AccountId, amount: U128, msg: String) -> PromiseOrValue<U128> {
        require!(env::predecessor_account_id() == self.token, "Only the game's coin is accepted");
        let amount = amount.0;
        let message: Message = match near_sdk::serde_json::from_str(&msg) {
            Ok(message) => message,
            Err(_) => env::panic_str("msg must be {\"heist\":{\"escape\":200}} or \"fund\""),
        };
        match message {
            Message::Fund => {
                self.vault += amount;
                log!("EVENT_JSON:{{\"standard\":\"heist\",\"version\":\"1.0.0\",\"event\":\"fund\",\"data\":[{{\"from\":\"{}\",\"amount\":\"{}\"}}]}}", sender_id, amount);
                PromiseOrValue::Value(U128(0))
            }
            Message::Heist { escape } => self.play(sender_id, amount, escape),
        }
    }

    fn play(&mut self, player: AccountId, bet: u128, escape: u32) -> PromiseOrValue<U128> {
        // A bet that cannot be played is refunded whole by the token (we return the full amount).
        if self.paused {
            log!("The vault is closed for now: bet refunded");
            return PromiseOrValue::Value(U128(bet));
        }
        if !(MIN_ESCAPE..=MAX_ESCAPE).contains(&escape) {
            log!("Escape must be between 1.01x and 1000x: bet refunded");
            return PromiseOrValue::Value(U128(bet));
        }
        if bet < self.min_bet {
            log!("Bet below the minimum: refunded");
            return PromiseOrValue::Value(U128(bet));
        }
        let payout = bet * escape as u128 / X;
        let profit = payout - bet;
        if profit > self.max_win() {
            log!("This heist could win more than the vault allows: bet refunded");
            return PromiseOrValue::Value(U128(bet));
        }

        let crash = roll(&env::random_seed(), &player, self.next_id, self.edge_bps);
        let won = crash >= escape;
        let burned = if won { 0 } else { bet / 2 };
        let heist = Heist {
            id: self.next_id,
            player: player.clone(),
            bet: U128(bet),
            escape,
            crash,
            won,
            payout: U128(if won { payout } else { 0 }),
            burned: U128(burned),
            block: env::block_height(),
            timestamp_ms: env::block_timestamp_ms(),
        };
        self.next_id += 1;
        self.record(&heist);

        log!(
            "EVENT_JSON:{}",
            near_sdk::serde_json::json!({"standard":"heist","version":"1.0.0","event":"heist","data":[heist]})
        );

        if won {
            self.vault -= profit;
            self.send(player, payout, "The Heist: you escaped with the loot");
        } else {
            // Half stays in the vault, half is burned.
            self.vault += bet - burned;
            self.send(self.burn_account.clone(), burned, "The Heist: burned by the Sheriff");
        }
        PromiseOrValue::Value(U128(0))
    }

    fn record(&mut self, heist: &Heist) {
        let bet = heist.bet.0;
        self.stats.heists += 1;
        self.stats.wagered = U128(self.stats.wagered.0 + bet);
        if heist.won {
            self.stats.escapes += 1;
            self.stats.paid = U128(self.stats.paid.0 + heist.payout.0);
        } else {
            self.stats.burned = U128(self.stats.burned.0 + heist.burned.0);
        }
        let mut player = self.players.get(&heist.player).cloned().unwrap_or_default();
        player.heists += 1;
        player.wagered += bet;
        if heist.won {
            player.escapes += 1;
            player.won += heist.payout.0;
            player.best_escape = player.best_escape.max(heist.escape);
        }
        self.players.insert(heist.player.clone(), player);

        // A ring of the last RECENT heists.
        if self.recent.len() < RECENT {
            self.recent.push(heist.clone());
        } else {
            let slot = (heist.id % RECENT as u64) as u32;
            self.recent.replace(slot, heist.clone());
        }
    }

    fn send(&mut self, to: AccountId, amount: u128, memo: &str) {
        if amount == 0 {
            return;
        }
        ext_ft::ext(self.token.clone())
            .with_attached_deposit(ONE_YOCTO)
            .with_static_gas(GAS_FT_TRANSFER)
            .ft_transfer(to.clone(), U128(amount), Some(memo.to_string()))
            .then(Self::ext(env::current_account_id()).with_static_gas(GAS_CALLBACK).on_sent(to, U128(amount)));
    }

    /// If a transfer fails (for example the winner is not registered with the coin), the coins are
    /// kept for them: `retry_owed` sends them again later.
    #[private]
    pub fn on_sent(&mut self, to: AccountId, amount: U128) -> bool {
        match env::promise_result(0) {
            PromiseResult::Successful(_) => true,
            _ => {
                let owed = self.owed.get(&to).copied().unwrap_or(0) + amount.0;
                self.owed.insert(to.clone(), owed);
                log!("Transfer to {} failed: {} kept for them", to, amount.0);
                false
            }
        }
    }

    /// Anyone can retry a failed transfer for an account (it only ever goes to that account).
    pub fn retry_owed(&mut self, account: AccountId) {
        let amount = self.owed.remove(&account).unwrap_or(0);
        require!(amount > 0, "Nothing owed to this account");
        self.send(account, amount, "The Heist: delivery retried");
    }

    // ---------- owner ----------

    pub fn set_paused(&mut self, paused: bool) {
        self.only_owner();
        self.paused = paused;
    }

    pub fn set_limits(&mut self, min_bet: Option<U128>, max_win_bps: Option<u16>) {
        self.only_owner();
        if let Some(min_bet) = min_bet {
            self.min_bet = min_bet.0;
        }
        if let Some(bps) = max_win_bps {
            require!(bps >= 10 && bps <= 1000, "Max win must be between 0.1% and 10% of the vault");
            self.max_win_bps = bps;
        }
    }

    /// The owner can take coins out of the vault, always in public (the event is on-chain).
    #[payable]
    pub fn withdraw_vault(&mut self, amount: U128, to: AccountId) {
        self.only_owner();
        near_sdk::assert_one_yocto();
        require!(amount.0 <= self.vault, "Not that much in the vault");
        self.vault -= amount.0;
        log!("EVENT_JSON:{{\"standard\":\"heist\",\"version\":\"1.0.0\",\"event\":\"withdraw\",\"data\":[{{\"to\":\"{}\",\"amount\":\"{}\"}}]}}", to, amount.0);
        self.send(to, amount.0, "The Heist: vault withdrawal");
    }

    fn only_owner(&self) {
        require!(env::predecessor_account_id() == self.owner, "Only the owner");
    }

    fn max_win(&self) -> u128 {
        self.vault * self.max_win_bps as u128 / 10_000
    }

    // ---------- views ----------

    pub fn get_config(&self) -> Config {
        Config {
            owner: self.owner.clone(),
            token: self.token.clone(),
            burn_account: self.burn_account.clone(),
            edge_bps: self.edge_bps,
            max_win_bps: self.max_win_bps,
            min_bet: U128(self.min_bet),
            vault: U128(self.vault),
            max_bet_at_2x: U128(self.max_bet(200).0),
            paused: self.paused,
        }
    }

    pub fn get_stats(&self) -> Stats {
        self.stats.clone()
    }

    /// The biggest bet accepted when running away at `escape` (×100).
    pub fn max_bet(&self, escape: u32) -> U128 {
        if escape <= 100 {
            return U128(0);
        }
        U128(self.max_win() * X / (escape as u128 - X))
    }

    /// Chance (in basis points) that a heist reaches `escape`: (1 - edge) / escape.
    pub fn win_chance_bps(&self, escape: u32) -> u32 {
        if escape < 100 {
            return 0;
        }
        ((10_000 - self.edge_bps as u64) * 100 / escape as u64) as u32
    }

    /// The last heists, newest first.
    pub fn get_recent(&self, limit: Option<u32>) -> Vec<Heist> {
        let mut all: Vec<Heist> = self.recent.iter().cloned().collect();
        all.sort_by(|a, b| b.id.cmp(&a.id));
        all.truncate(limit.unwrap_or(RECENT).min(RECENT) as usize);
        all
    }

    pub fn get_player(&self, account: AccountId) -> PlayerStats {
        let p = self.players.get(&account).cloned().unwrap_or_default();
        PlayerStats { heists: p.heists, escapes: p.escapes, wagered: U128(p.wagered), won: U128(p.won), best_escape: p.best_escape }
    }

    pub fn get_owed(&self, account: AccountId) -> U128 {
        U128(self.owed.get(&account).copied().unwrap_or(0))
    }
}

/// The crash point (×100) from the block's random seed, the player and the heist number.
/// With r uniform in [0, 2^64): crash = (1 - edge) / (1 - r / 2^64), floored to 0.01x, at least 1.00x.
/// So P(crash >= x) = (1 - edge) / x for any x >= 1.
pub fn roll(seed: &[u8], player: &AccountId, id: u64, edge_bps: u16) -> u32 {
    let mut input = seed.to_vec();
    input.extend_from_slice(player.as_bytes());
    input.extend_from_slice(&id.to_le_bytes());
    let hash = env::sha256(&input);
    let r = u64::from_le_bytes(hash[..8].try_into().unwrap()) as u128;
    crash_from(r, edge_bps)
}

pub fn crash_from(r: u128, edge_bps: u16) -> u32 {
    const TWO64: u128 = 1u128 << 64;
    // (10000 - edge)/10000 * 2^64 / (2^64 - r) * 100, all in integers.
    let num = (10_000 - edge_bps as u128) * TWO64;
    let den = (TWO64 - r) * 100;
    let crash = num / den;
    crash.clamp(100, MAX_CRASH as u128) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use near_sdk::test_utils::{accounts, get_logs, VMContextBuilder};
    use near_sdk::testing_env;

    const COIN: u128 = 1_000_000_000_000_000_000; // 1 coin at 18 decimals

    fn token() -> AccountId { "coin.testnet".parse().unwrap() }
    fn ctx(predecessor: AccountId, seed: u8) -> VMContextBuilder {
        let mut b = VMContextBuilder::new();
        b.current_account_id("heist.testnet".parse().unwrap())
            .predecessor_account_id(predecessor)
            .random_seed([seed; 32])
            .block_height(100 + seed as u64);
        b
    }
    fn game() -> Contract {
        testing_env!(ctx(accounts(0), 1).build());
        let mut c = Contract::new(accounts(0), token(), "burn.testnet".parse().unwrap(), None, None, U128(COIN));
        testing_env!(ctx(token(), 1).build());
        c.ft_on_transfer(accounts(0), U128(1_000_000 * COIN), "\"fund\"".into());
        c
    }
    fn refund(v: PromiseOrValue<U128>) -> u128 {
        match v { PromiseOrValue::Value(v) => v.0, _ => panic!("expected a value") }
    }

    #[test]
    fn crash_math_matches_the_odds() {
        // r = 0 gives the lowest crash: 0.97x, floored to 1.00x.
        assert_eq!(crash_from(0, 300), 100);
        // r = half of the range: 0.97 / 0.5 = 1.94x.
        assert_eq!(crash_from(1u128 << 63, 300), 194);
        // Near the top it is capped.
        assert_eq!(crash_from((1u128 << 64) - 1, 300), MAX_CRASH);
        // Empirical check over many evenly spread r: the share reaching 2x is about 48.5%.
        let n = 100_000u128;
        let step = (1u128 << 64) / n;
        let wins = (0..n).filter(|i| crash_from(i * step, 300) >= 200).count() as f64;
        let share = wins / n as f64;
        assert!((share - 0.485).abs() < 0.002, "share {share}");
        let wins10 = (0..n).filter(|i| crash_from(i * step, 300) >= 1000).count() as f64 / n as f64;
        assert!((wins10 - 0.097).abs() < 0.002, "share10 {wins10}");
    }

    #[test]
    fn only_the_coin_can_call() {
        let mut c = game();
        testing_env!(ctx(accounts(2), 1).build());
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            c.ft_on_transfer(accounts(1), U128(COIN), "{\"heist\":{\"escape\":200}}".into());
        }));
        assert!(r.is_err());
    }

    #[test]
    fn a_heist_wins_or_burns_and_the_books_balance() {
        let mut c = game();
        let start = c.vault;
        let mut won = 0u32; let mut lost = 0u32;
        for seed in 0..60u8 {
            testing_env!(ctx(token(), seed).build());
            let before = c.vault;
            assert_eq!(refund(c.ft_on_transfer(accounts(1), U128(10 * COIN), "{\"heist\":{\"escape\":200}}".into())), 0);
            let h = c.get_recent(Some(1)).pop().unwrap();
            if h.won {
                won += 1;
                assert_eq!(h.payout.0, 20 * COIN);
                assert_eq!(c.vault, before - 10 * COIN);
                assert!(h.crash >= 200);
            } else {
                lost += 1;
                assert_eq!(h.burned.0, 5 * COIN);
                assert_eq!(c.vault, before + 5 * COIN);
                assert!(h.crash < 200);
            }
            assert!(get_logs().iter().any(|l| l.starts_with("EVENT_JSON:") && l.contains("\"event\":\"heist\"")));
        }
        assert!(won > 0 && lost > 0);
        let s = c.get_stats();
        assert_eq!(s.heists, 60);
        assert_eq!(s.escapes as u32, won);
        assert_eq!(s.wagered.0, 600 * COIN);
        assert_eq!(s.burned.0, lost as u128 * 5 * COIN);
        // vault = start - profits paid + half of each lost bet
        assert_eq!(c.vault, start - won as u128 * 10 * COIN + lost as u128 * 5 * COIN);
        let p = c.get_player(accounts(1));
        assert_eq!(p.heists, 60);
        assert_eq!(p.escapes as u32, won);
    }

    #[test]
    fn unplayable_bets_are_refunded() {
        let mut c = game();
        testing_env!(ctx(token(), 3).build());
        // Too small.
        assert_eq!(refund(c.ft_on_transfer(accounts(1), U128(COIN / 2), "{\"heist\":{\"escape\":200}}".into())), COIN / 2);
        // Escape out of range.
        assert_eq!(refund(c.ft_on_transfer(accounts(1), U128(COIN), "{\"heist\":{\"escape\":100}}".into())), COIN);
        // Could win more than 1% of the vault (10,000 coins): 20,000 at 2x would profit 20,000.
        assert_eq!(refund(c.ft_on_transfer(accounts(1), U128(20_000 * COIN), "{\"heist\":{\"escape\":200}}".into())), 20_000 * COIN);
        // The biggest allowed bet at 2x is exactly 10,000 coins.
        assert_eq!(c.max_bet(200).0, 10_000 * COIN);
        assert_eq!(refund(c.ft_on_transfer(accounts(1), U128(10_000 * COIN), "{\"heist\":{\"escape\":200}}".into())), 0);
        assert_eq!(c.get_stats().heists, 1);
    }

    #[test]
    fn paused_vault_refunds() {
        let mut c = game();
        testing_env!(ctx(accounts(0), 1).build());
        c.set_paused(true);
        testing_env!(ctx(token(), 1).build());
        assert_eq!(refund(c.ft_on_transfer(accounts(1), U128(COIN), "{\"heist\":{\"escape\":150}}".into())), COIN);
    }

    #[test]
    fn recent_keeps_the_last_fifty_newest_first() {
        let mut c = game();
        for i in 0..70u32 {
            testing_env!(ctx(token(), (i % 250) as u8).build());
            c.ft_on_transfer(accounts(1), U128(COIN), "{\"heist\":{\"escape\":150}}".into());
        }
        let r = c.get_recent(None);
        assert_eq!(r.len(), 50);
        assert_eq!(r[0].id, 69);
        assert_eq!(r[49].id, 20);
    }

    #[test]
    fn win_chance_matches_the_formula() {
        let c = game();
        assert_eq!(c.win_chance_bps(200), 4850);
        assert_eq!(c.win_chance_bps(1000), 970);
    }
}
