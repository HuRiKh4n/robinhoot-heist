//! RobinHoot: The Kingdom.
//!
//! A castle with three gates guards the Sheriff's treasury. Outlaws (players) steal from it and
//! sabotage its gates; the Sheriff, an AI agent running on NEAR AI Cloud, answers every round by
//! spending part of the treasury on defenses. The Sheriff's decisions are only applied if:
//!   1. they are signed by a NEAR AI Cloud TEE signing key on the allowlist (ECDSA, Ethereum-style),
//!   2. the signed request contains the exact digest of this round's on-chain state,
//!   3. they obey the Crown's rules (valid action, valid gate, budget within 5% of the treasury).
//! A decision that breaks a rule is recorded and rejected: the Sheriff loses his turn.
//!
//! Every move of the war burns the memecoin: failed steals burn half the stake, sabotage burns the
//! whole bribe, and the Sheriff's spending on guards is burned too.

use near_sdk::borsh::{BorshDeserialize, BorshSerialize};
use near_sdk::json_types::U128;
use near_sdk::serde::{Deserialize, Serialize};
use near_sdk::serde_json::{self, Value};
use near_sdk::store::{LookupMap, Vector};
use near_sdk::{env, ext_contract, log, near, require, AccountId, BorshStorageKey, Gas, NearToken, PanicOnDefault, PromiseOrValue};

pub const GATES: [&str; 3] = ["north", "east", "south"];
pub const CLASSES: [&str; 3] = ["rogue", "warrior", "ranger"];
pub const BPS: u128 = 10_000;
pub const MAX_DEFENSE: u32 = 9_000; // 90%
const RECENT: u32 = 50;
const LOG: u32 = 30;
const EDGE_BPS: u128 = 300; // steal odds are (1 - defense) × 97%
const SHERIFF_BUDGET_BPS: u128 = 500; // the Sheriff may spend at most 5% of the treasury per turn
const MAX_WIN_BPS: u128 = 100; // one steal can take at most 1% of the treasury
const ONE_YOCTO: NearToken = NearToken::from_yoctonear(1);

#[derive(BorshSerialize, BorshStorageKey)]
#[borsh(crate = "near_sdk::borsh")]
enum Key { Balances, Outlaws, Actions, Log }

#[derive(BorshSerialize, BorshDeserialize, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[borsh(crate = "near_sdk::borsh")]
#[serde(crate = "near_sdk::serde")]
pub struct Gate {
    pub name: String,
    /// Chance (bps) that the guards stop a thief. 5000 = 50%.
    pub defense: u32,
    pub steals_won: u32,
    pub steals_lost: u32,
    pub sabotaged_this_round: U128,
    pub fortified_this_round: U128,
}

#[derive(BorshSerialize, BorshDeserialize, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[borsh(crate = "near_sdk::borsh")]
#[serde(crate = "near_sdk::serde")]
pub struct Outlaw {
    pub name: String,
    pub class: String,
    pub steals: u32,
    pub steals_won: u32,
    pub stolen: U128,
    pub sabotaged: U128,
}

/// One outlaw move, as shown in the tavern feed.
#[derive(BorshSerialize, BorshDeserialize, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[borsh(crate = "near_sdk::borsh")]
#[serde(crate = "near_sdk::serde")]
pub struct Action {
    pub id: u64,
    pub round: u64,
    pub player: AccountId,
    pub kind: String, // "steal" | "sabotage"
    pub gate: String,
    pub stake: U128,
    pub won: bool,
    /// steal: coins taken from the treasury; sabotage: defense points removed (bps)
    pub amount: U128,
    pub burned: U128,
    pub chance_bps: u32,
    pub timestamp_ms: u64,
}

/// One Sheriff turn: what the AI decided, whether the Crown's rules accepted it, and the proof.
#[derive(BorshSerialize, BorshDeserialize, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[borsh(crate = "near_sdk::borsh")]
#[serde(crate = "near_sdk::serde")]
pub struct SheriffMove {
    pub round: u64,
    pub action: String,
    pub gate: Option<String>,
    pub budget: U128,
    pub reason: String,
    pub accepted: bool,
    pub verdict: String,
    pub signer: String,
    pub request_hash: String,
    pub response_hash: String,
    pub state_digest: String,
    pub timestamp_ms: u64,
}

#[derive(BorshSerialize, BorshDeserialize, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[borsh(crate = "near_sdk::borsh")]
#[serde(crate = "near_sdk::serde")]
pub struct WarStats {
    pub steals: u64,
    pub steals_won: u64,
    pub stolen: U128,
    pub sabotages: u64,
    pub burned_by_outlaws: U128,
    pub burned_by_sheriff: U128,
    pub sheriff_turns: u64,
    pub sheriff_rejected: u64,
}

impl Default for WarStats {
    fn default() -> Self {
        Self { steals: 0, steals_won: 0, stolen: U128(0), sabotages: 0, burned_by_outlaws: U128(0), burned_by_sheriff: U128(0), sheriff_turns: 0, sheriff_rejected: 0 }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(crate = "near_sdk::serde")]
pub struct KingdomView {
    pub round: u64,
    pub round_started_ms: u64,
    pub round_ms: u64,
    pub treasury: U128,
    pub gates: Vec<Gate>,
    pub stats: WarStats,
    pub pending_burn: U128,
    pub token: AccountId,
    pub burn_account: AccountId,
    pub model: String,
    pub signers: Vec<String>,
    pub max_steal: U128,
    pub sheriff_budget: U128,
    pub outlaws: u64,
}

/// The decision format the Sheriff must answer with (inside the model's message content).
#[derive(Deserialize, Debug)]
#[serde(crate = "near_sdk::serde")]
struct Decision {
    action: String,
    #[serde(default)]
    gate: Option<String>,
    #[serde(default)]
    budget: Option<Value>,
    #[serde(default)]
    reason: Option<String>,
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
    decimals: u8,
    min_stake: u128,
    round_ms: u64,
    model: String,
    /// NEAR AI Cloud TEE signing addresses (lowercase hex, no 0x), checked against attestation off-chain.
    signers: Vec<String>,
    round: u64,
    round_started_ms: u64,
    treasury: u128,
    gates: Vec<Gate>,
    stats: WarStats,
    pending_burn: u128,
    balances: LookupMap<AccountId, u128>,
    outlaws: LookupMap<AccountId, Outlaw>,
    outlaw_count: u64,
    actions: Vector<Action>,
    next_action: u64,
    log: Vector<SheriffMove>,
    log_count: u64,
}

#[near]
impl Contract {
    #[init]
    pub fn new(owner: AccountId, token: AccountId, burn_account: AccountId, decimals: u8, min_stake: U128, round_ms: u64, model: String, signers: Vec<String>) -> Self {
        require!(round_ms >= 60_000, "Rounds last at least a minute");
        Self {
            owner, token, burn_account, decimals, min_stake: min_stake.0, round_ms, model,
            signers: signers.into_iter().map(|s| normalize_address(&s)).collect(),
            round: 1,
            round_started_ms: env::block_timestamp_ms(),
            treasury: 0,
            gates: GATES.iter().map(|g| Gate { name: g.to_string(), defense: 5_000, steals_won: 0, steals_lost: 0, sabotaged_this_round: U128(0), fortified_this_round: U128(0) }).collect(),
            stats: WarStats::default(),
            pending_burn: 0,
            balances: LookupMap::new(Key::Balances),
            outlaws: LookupMap::new(Key::Outlaws),
            outlaw_count: 0,
            actions: Vector::new(Key::Actions),
            next_action: 0,
            log: Vector::new(Key::Log),
            log_count: 0,
        }
    }

    // ---------------- coins in and out ----------------

    /// `"deposit"`: coins into your pouch (then play without wallet pop-ups). `"fund"`: into the treasury.
    pub fn ft_on_transfer(&mut self, sender_id: AccountId, amount: U128, msg: String) -> PromiseOrValue<U128> {
        require!(env::predecessor_account_id() == self.token, "Only the kingdom's coin is accepted");
        match msg.trim_matches('"') {
            "deposit" => { let b = self.balance(&sender_id) + amount.0; self.balances.insert(sender_id, b); }
            "fund" => { self.treasury += amount.0; }
            _ => env::panic_str("msg must be \"deposit\" or \"fund\""),
        }
        PromiseOrValue::Value(U128(0))
    }

    /// Take coins from your pouch back to your wallet.
    pub fn withdraw(&mut self, amount: U128) {
        let player = env::predecessor_account_id();
        let b = self.balance(&player);
        require!(amount.0 > 0 && amount.0 <= b, "Not that much in your pouch");
        self.balances.insert(player.clone(), b - amount.0);
        self.transfer(player, amount.0, "The Kingdom: withdrawal");
    }

    // ---------------- outlaws ----------------

    /// Join the Guild with a name and a class: rogue (+5 points on steals), warrior (sabotage ×1.5),
    /// ranger (+2 points on steals, sabotage ×1.2).
    pub fn join(&mut self, name: String, class: String) {
        let player = env::predecessor_account_id();
        let name: String = name.trim().chars().filter(|c| !c.is_control()).take(24).collect();
        require!(!name.is_empty(), "Your outlaw needs a name");
        require!(CLASSES.contains(&class.as_str()), "Class must be rogue, warrior or ranger");
        match self.outlaws.get(&player).cloned() {
            Some(mut o) => { o.name = name; o.class = class; self.outlaws.insert(player, o); }
            None => {
                self.outlaws.insert(player, Outlaw { name, class, steals: 0, steals_won: 0, stolen: U128(0), sabotaged: U128(0) });
                self.outlaw_count += 1;
            }
        }
    }

    /// Steal from the treasury through a gate. Win: you take your stake again from the treasury
    /// (stake × 2 back). Lose: half the stake is burned, half goes to the treasury.
    /// Odds = (100% − gate defense) × 97%, plus your class bonus.
    pub fn steal(&mut self, gate: String, stake: U128) -> Action {
        let player = env::predecessor_account_id();
        let outlaw = self.outlaws.get(&player).cloned().unwrap_or_else(|| env::panic_str("Join the Guild first"));
        let g = gate_index(&gate);
        let stake = stake.0;
        self.spend(&player, stake);
        require!(stake <= self.max_steal(), "The treasury cannot cover that steal: stake less");

        let bonus: u128 = match outlaw.class.as_str() { "rogue" => 500, "ranger" => 200, _ => 0 };
        let defense = self.gates[g].defense as u128;
        let chance = ((BPS - defense) * (BPS - EDGE_BPS) / BPS + bonus).min(9_500);
        let roll = roll_bps(&player, self.next_action);
        let won = roll < chance;

        let (amount, burned) = if won {
            self.treasury -= stake;
            let b = self.balance(&player) + 2 * stake;
            self.balances.insert(player.clone(), b);
            self.gates[g].steals_won += 1;
            (stake, 0)
        } else {
            let burned = stake / 2;
            self.treasury += stake - burned;
            self.pending_burn += burned;
            self.gates[g].steals_lost += 1;
            (0, burned)
        };
        self.stats.steals += 1;
        if won { self.stats.steals_won += 1; self.stats.stolen = U128(self.stats.stolen.0 + amount); }
        self.stats.burned_by_outlaws = U128(self.stats.burned_by_outlaws.0 + burned);
        let mut o = outlaw;
        o.steals += 1;
        if won { o.steals_won += 1; o.stolen = U128(o.stolen.0 + amount); }
        self.outlaws.insert(player.clone(), o);
        self.record(player, "steal", g, stake, won, amount, burned, chance as u32)
    }

    /// Bribe the guards of a gate: the whole bribe is burned and the gate's defense drops for everyone.
    /// A bribe of 1% of the treasury removes 10 points of defense (warriors ×1.5, rangers ×1.2).
    pub fn sabotage(&mut self, gate: String, stake: U128) -> Action {
        let player = env::predecessor_account_id();
        let outlaw = self.outlaws.get(&player).cloned().unwrap_or_else(|| env::panic_str("Join the Guild first"));
        let g = gate_index(&gate);
        let stake = stake.0;
        self.spend(&player, stake);
        let mult: u128 = match outlaw.class.as_str() { "warrior" => 150, "ranger" => 120, _ => 100 };
        let drop = (self.points_for(stake) * mult / 100).min(self.gates[g].defense as u128) as u32;
        self.gates[g].defense -= drop;
        self.gates[g].sabotaged_this_round = U128(self.gates[g].sabotaged_this_round.0 + stake);
        self.pending_burn += stake;
        self.stats.sabotages += 1;
        self.stats.burned_by_outlaws = U128(self.stats.burned_by_outlaws.0 + stake);
        let mut o = outlaw;
        o.sabotaged = U128(o.sabotaged.0 + stake);
        self.outlaws.insert(player.clone(), o);
        self.record(player, "sabotage", g, stake, true, drop as u128, stake, 0)
    }

    // ---------------- the Sheriff ----------------

    /// Submit the Sheriff's decision for this round. Anyone can submit it (the worker, a player, a judge):
    /// what makes it valid is NEAR AI Cloud's signature, not who sends it.
    /// `signed_text` is NEAR AI's signed text (`…:sha256(request):sha256(response)`), `signature` its
    /// 65-byte hex ECDSA signature, `request_body`/`response_body` the exact bytes sent and received.
    pub fn sheriff_turn(&mut self, request_body: String, response_body: String, signed_text: String, signature: String) -> SheriffMove {
        require!(env::block_timestamp_ms() >= self.round_started_ms + self.round_ms, "The round is not over yet");
        let request_hash = hex(&env::sha256(request_body.as_bytes()));
        let response_hash = hex(&env::sha256(response_body.as_bytes()));
        let parts: Vec<&str> = signed_text.split(':').collect();
        require!(parts.len() >= 2 && parts[parts.len() - 2] == request_hash && parts[parts.len() - 1] == response_hash, "Signed text does not match the request and response");
        let signer = recover_eth_address(&signed_text, &signature).unwrap_or_else(|| env::panic_str("Bad signature"));
        require!(self.signers.contains(&signer), "Not signed by an allowed NEAR AI Cloud key");
        let digest = self.state_digest();
        require!(request_body.contains(&format!("STATE_DIGEST={digest}")), "The Sheriff did not see this round's state");
        require!(request_body.contains(&format!("\"model\":\"{}\"", self.model)) || request_body.contains(&format!("\"model\": \"{}\"", self.model)), "Wrong model");

        let mut mv = SheriffMove {
            round: self.round, action: String::new(), gate: None, budget: U128(0), reason: String::new(),
            accepted: false, verdict: String::new(), signer, request_hash, response_hash, state_digest: digest,
            timestamp_ms: env::block_timestamp_ms(),
        };
        match parse_decision(&response_body) {
            Err(e) => { mv.action = "UNREADABLE".into(); mv.verdict = e; }
            Ok(d) => {
                mv.action = d.action.to_uppercase();
                mv.gate = d.gate.clone().map(|g| g.to_lowercase());
                mv.reason = d.reason.unwrap_or_default().chars().take(240).collect();
                let budget = d.budget.as_ref().and_then(coins_from_json).map(|c| c.saturating_mul(self.unit()));
                mv.budget = U128(budget.unwrap_or(0));
                mv.verdict = match self.apply(&mv.action, mv.gate.as_deref(), budget) {
                    Ok(v) => { mv.accepted = true; v }
                    Err(e) => e,
                };
            }
        }
        self.stats.sheriff_turns += 1;
        if !mv.accepted { self.stats.sheriff_rejected += 1; }
        log!("EVENT_JSON:{}", serde_json::json!({"standard":"kingdom","version":"1.0.0","event":"sheriff_turn","data":[mv]}));
        self.push_log(mv.clone());
        self.next_round();
        mv
    }

    /// If the Sheriff sleeps (no signed decision for two rounds), anyone can move the kingdom on.
    pub fn skip_turn(&mut self) {
        require!(env::block_timestamp_ms() >= self.round_started_ms + 2 * self.round_ms, "Give the Sheriff time to answer");
        let mv = SheriffMove {
            round: self.round, action: "SLEEP".into(), gate: None, budget: U128(0), reason: "No signed decision arrived".into(),
            accepted: false, verdict: "The Sheriff slept through the round".into(), signer: String::new(),
            request_hash: String::new(), response_hash: String::new(), state_digest: self.state_digest(), timestamp_ms: env::block_timestamp_ms(),
        };
        self.push_log(mv);
        self.next_round();
    }

    // ---------------- owner: only the NEAR AI signer allowlist ----------------

    /// The owner can only update which NEAR AI Cloud signing keys are trusted (after checking their
    /// TDX attestation off-chain) and the model name. It can never touch the treasury or balances.
    pub fn set_signers(&mut self, signers: Vec<String>, model: Option<String>) {
        require!(env::predecessor_account_id() == self.owner, "Only the owner");
        self.signers = signers.into_iter().map(|s| normalize_address(&s)).collect();
        if let Some(m) = model { self.model = m; }
    }

    /// Send burned coins to the burn address (anyone can call; also happens at every new round).
    pub fn flush_burn(&mut self) {
        let amount = self.pending_burn;
        if amount > 0 {
            self.pending_burn = 0;
            let to = self.burn_account.clone();
            self.transfer(to, amount, "The Kingdom: burned in the war");
        }
    }

    // ---------------- views ----------------

    pub fn get_kingdom(&self) -> KingdomView {
        KingdomView {
            round: self.round, round_started_ms: self.round_started_ms, round_ms: self.round_ms,
            treasury: U128(self.treasury), gates: self.gates.clone(), stats: self.stats.clone(),
            pending_burn: U128(self.pending_burn), token: self.token.clone(), burn_account: self.burn_account.clone(),
            model: self.model.clone(), signers: self.signers.clone(), max_steal: U128(self.max_steal()),
            sheriff_budget: U128(self.treasury * SHERIFF_BUDGET_BPS / BPS), outlaws: self.outlaw_count,
        }
    }
    pub fn get_balance(&self, account: AccountId) -> U128 { U128(self.balance(&account)) }
    pub fn get_outlaw(&self, account: AccountId) -> Option<Outlaw> { self.outlaws.get(&account).cloned() }
    pub fn get_actions(&self, limit: Option<u32>) -> Vec<Action> { newest(&self.actions, limit, RECENT, |a| a.id) }
    pub fn get_log(&self, limit: Option<u32>) -> Vec<SheriffMove> { newest(&self.log, limit, LOG, |m| m.round) }

    /// The text the Sheriff must read this round. Its last line carries the digest the contract checks.
    pub fn state_prompt(&self) -> String {
        let u = self.unit();
        let mut s = format!("ROUND {}\nTREASURY {} coins\nYOUR BUDGET THIS TURN: at most {} coins\n", self.round, self.treasury / u, self.treasury * SHERIFF_BUDGET_BPS / BPS / u);
        for g in &self.gates {
            s += &format!("GATE {}: defense {}%, steals won {}, steals lost {}, bribes this round {} coins\n", g.name, g.defense / 100, g.steals_won, g.steals_lost, g.sabotaged_this_round.0 / u);
        }
        s += &format!("OUTLAWS IN THE GUILD {}\nSTATE_DIGEST={}", self.outlaw_count, self.state_digest());
        s
    }

    /// sha256 of the canonical state of this round (round, treasury, gate defenses).
    pub fn state_digest(&self) -> String {
        let mut canon = format!("{}|{}", self.round, self.treasury);
        for g in &self.gates { canon += &format!("|{}:{}", g.name, g.defense); }
        hex(&env::sha256(canon.as_bytes()))[..32].to_string()
    }

    // ---------------- internals ----------------

    /// Apply a decision under the Crown's rules. Spending is burned (paid to guards who leave the kingdom).
    fn apply(&mut self, action: &str, gate: Option<&str>, budget: Option<u128>) -> Result<String, String> {
        let cap = self.treasury * SHERIFF_BUDGET_BPS / BPS;
        let u = self.unit();
        match action {
            "HOARD" => Ok("The Sheriff keeps his gold".into()),
            "FORTIFY" | "HIRE_GUARDS" => {
                let budget = budget.ok_or("Rejected: no budget given")?;
                if budget == 0 { return Err("Rejected: budget must be above zero".into()); }
                if budget > cap { return Err(format!("Rejected by the Crown: budget {} is over the limit of {} coins (5% of the treasury)", budget / u, cap / u)); }
                let points = self.points_for(budget) as u32;
                if action == "FORTIFY" {
                    let g = gate.and_then(|g| GATES.iter().position(|x| *x == g)).ok_or("Rejected: unknown gate")?;
                    self.gates[g].defense = (self.gates[g].defense + points).min(MAX_DEFENSE);
                    self.gates[g].fortified_this_round = U128(self.gates[g].fortified_this_round.0 + budget);
                } else {
                    for g in self.gates.iter_mut() { g.defense = (g.defense + points / 3).min(MAX_DEFENSE); }
                }
                self.treasury -= budget;
                self.pending_burn += budget;
                self.stats.burned_by_sheriff = U128(self.stats.burned_by_sheriff.0 + budget);
                Ok(format!("Accepted: {} coins spent on guards (+{} defense points)", budget / u, points / 100))
            }
            other => Err(format!("Rejected: unknown action {other}")),
        }
    }

    /// Defense points (bps) bought by `coins`: 1% of the treasury = 10 points (1000 bps).
    fn points_for(&self, coins: u128) -> u128 {
        if self.treasury == 0 { return 0; }
        coins * 100_000 / self.treasury
    }
    fn max_steal(&self) -> u128 { self.treasury * MAX_WIN_BPS / BPS }
    fn unit(&self) -> u128 { 10u128.pow(self.decimals as u32) }
    fn balance(&self, a: &AccountId) -> u128 { self.balances.get(a).copied().unwrap_or(0) }
    fn spend(&mut self, player: &AccountId, stake: u128) {
        require!(stake >= self.min_stake, "Stake below the minimum");
        let b = self.balance(player);
        require!(stake <= b, "Not enough coins in your pouch: deposit first");
        self.balances.insert(player.clone(), b - stake);
    }

    #[allow(clippy::too_many_arguments)]
    fn record(&mut self, player: AccountId, kind: &str, g: usize, stake: u128, won: bool, amount: u128, burned: u128, chance: u32) -> Action {
        let a = Action {
            id: self.next_action, round: self.round, player, kind: kind.into(), gate: GATES[g].into(),
            stake: U128(stake), won, amount: U128(amount), burned: U128(burned), chance_bps: chance,
            timestamp_ms: env::block_timestamp_ms(),
        };
        self.next_action += 1;
        if self.actions.len() < RECENT { self.actions.push(a.clone()); } else { self.actions.replace((a.id % RECENT as u64) as u32, a.clone()); }
        log!("EVENT_JSON:{}", serde_json::json!({"standard":"kingdom","version":"1.0.0","event":kind,"data":[a]}));
        a
    }
    fn push_log(&mut self, mv: SheriffMove) {
        let n = self.log_count;
        self.log_count += 1;
        if self.log.len() < LOG { self.log.push(mv); } else { self.log.replace((n % LOG as u64) as u32, mv); }
    }
    fn next_round(&mut self) {
        self.round += 1;
        self.round_started_ms = env::block_timestamp_ms();
        for g in self.gates.iter_mut() { g.sabotaged_this_round = U128(0); g.fortified_this_round = U128(0); }
        self.flush_burn();
    }
    fn transfer(&mut self, to: AccountId, amount: u128, memo: &str) {
        let _ = ext_ft::ext(self.token.clone()).with_attached_deposit(ONE_YOCTO).with_static_gas(Gas::from_tgas(10))
            .ft_transfer(to, U128(amount), Some(memo.to_string()));
    }
}

fn gate_index(gate: &str) -> usize {
    GATES.iter().position(|g| *g == gate.to_lowercase()).unwrap_or_else(|| env::panic_str("Gate must be north, east or south"))
}

fn roll_bps(player: &AccountId, nonce: u64) -> u128 {
    let mut input = env::random_seed();
    input.extend_from_slice(player.as_bytes());
    input.extend_from_slice(&nonce.to_le_bytes());
    let h = env::sha256(&input);
    (u64::from_le_bytes(h[..8].try_into().unwrap()) % BPS as u64) as u128
}

fn newest<T: Clone + BorshSerialize + BorshDeserialize>(v: &Vector<T>, limit: Option<u32>, max: u32, key: impl Fn(&T) -> u64) -> Vec<T> {
    let mut all: Vec<T> = v.iter().cloned().collect();
    all.sort_by_key(|x| std::cmp::Reverse(key(x)));
    all.truncate(limit.unwrap_or(max).min(max) as usize);
    all
}

pub fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }
fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim().trim_start_matches("0x");
    if s.len() % 2 != 0 { return None; }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}
pub fn normalize_address(a: &str) -> String { a.trim().trim_start_matches("0x").to_lowercase() }

/// Ethereum personal_sign recovery: keccak256("\x19Ethereum Signed Message:\n" + len + text).
pub fn recover_eth_address(text: &str, signature_hex: &str) -> Option<String> {
    let sig = unhex(signature_hex)?;
    if sig.len() != 65 { return None; }
    let v = sig[64];
    let v = if v >= 27 { v - 27 } else { v };
    let msg = format!("\x19Ethereum Signed Message:\n{}{}", text.len(), text);
    let hash = env::keccak256(msg.as_bytes());
    let pubkey = env::ecrecover(&hash, &sig[..64], v, true)?;
    let addr = env::keccak256(&pubkey);
    Some(hex(&addr[12..]))
}

/// Pull the decision JSON out of an OpenAI-style chat completion body.
fn parse_decision(response_body: &str) -> Result<Decision, String> {
    let body: Value = serde_json::from_str(response_body).map_err(|_| "Unreadable: response is not JSON".to_string())?;
    let content = body["choices"][0]["message"]["content"].as_str().ok_or("Unreadable: no message content")?;
    let start = content.find('{').ok_or("Unreadable: no JSON decision in the answer")?;
    let end = content.rfind('}').ok_or("Unreadable: no JSON decision in the answer")?;
    serde_json::from_str::<Decision>(&content[start..=end]).map_err(|_| "Unreadable: decision JSON is malformed".to_string())
}

fn coins_from_json(v: &Value) -> Option<u128> {
    match v {
        Value::Number(n) => n.as_u64().map(|x| x as u128).or_else(|| n.as_f64().filter(|f| *f >= 0.0).map(|f| f as u128)),
        Value::String(s) => s.trim().parse::<u128>().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
