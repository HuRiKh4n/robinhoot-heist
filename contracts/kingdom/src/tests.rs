use super::*;
use k256::ecdsa::SigningKey;
use k256::elliptic_curve::sec1::ToEncodedPoint;
use near_sdk::test_utils::{accounts, get_logs, VMContextBuilder};
use near_sdk::testing_env;

const COIN: u128 = 1_000_000_000_000_000_000;
const ROUND: u64 = 600_000;
const MODEL: &str = "test-tee-model";

fn token() -> AccountId { "coin.testnet".parse().unwrap() }
fn ctx(pred: AccountId, seed: u8, ms: u64) -> VMContextBuilder {
    let mut b = VMContextBuilder::new();
    b.current_account_id("kingdom.testnet".parse().unwrap()).predecessor_account_id(pred)
        .random_seed([seed; 32]).block_timestamp(ms * 1_000_000);
    b
}

/// A stand-in for a NEAR AI Cloud TEE key: signs like `personal_sign`.
struct Tee { key: SigningKey }
impl Tee {
    fn new(byte: u8) -> Self { Self { key: SigningKey::from_slice(&[byte; 32]).unwrap() } }
    fn address(&self) -> String {
        let point = self.key.verifying_key().to_encoded_point(false);
        hex(&env::keccak256(&point.as_bytes()[1..])[12..])
    }
    fn sign(&self, text: &str) -> String {
        let msg = format!("\x19Ethereum Signed Message:\n{}{}", text.len(), text);
        let hash = env::keccak256(msg.as_bytes());
        let (sig, rec) = self.key.sign_prehash_recoverable(&hash).unwrap();
        let mut bytes = sig.to_bytes().to_vec();
        bytes.push(rec.to_byte() + 27);
        format!("0x{}", hex(&bytes))
    }
}

fn kingdom(tee: &Tee) -> Contract {
    testing_env!(ctx(accounts(0), 1, 1_000).build());
    let mut c = Contract::new(accounts(0), token(), "burn.testnet".parse().unwrap(), 18, U128(COIN), ROUND, MODEL.into(), vec![format!("0x{}", tee.address().to_uppercase())]);
    testing_env!(ctx(token(), 1, 1_000).build());
    let _ = c.ft_on_transfer(accounts(0), U128(1_000_000 * COIN), "\"fund\"".into());
    let _ = c.ft_on_transfer(accounts(1), U128(100_000 * COIN), "\"deposit\"".into());
    testing_env!(ctx(accounts(1), 1, 1_000).build());
    c.join("Robin".into(), "rogue".into());
    c
}

/// What the worker sends: the chat request (with the state prompt) and NEAR AI's response.
fn sheriff_call(c: &Contract, tee: &Tee, decision: &str) -> (String, String, String, String) {
    let request = serde_json::json!({"model": MODEL, "messages": [{"role":"user","content": c.state_prompt()}]}).to_string();
    let response = serde_json::json!({"id":"chat-1","choices":[{"message":{"role":"assistant","content": decision}}]}).to_string();
    let text = format!("{}:{}:{}", MODEL, hex(&env::sha256(request.as_bytes())), hex(&env::sha256(response.as_bytes())));
    let sig = tee.sign(&text);
    (request, response, text, sig)
}

#[test]
fn signature_recovery_matches_the_tee_address() {
    let tee = Tee::new(7);
    testing_env!(ctx(accounts(0), 1, 0).build());
    let sig = tee.sign("hello:aa:bb");
    assert_eq!(recover_eth_address("hello:aa:bb", &sig).unwrap(), tee.address());
    assert_ne!(recover_eth_address("hello:aa:bc", &sig).unwrap(), tee.address());
}

#[test]
fn steals_and_sabotage_change_the_shared_castle() {
    let tee = Tee::new(7);
    let mut c = kingdom(&tee);
    // 10,000 coins = 1% of the treasury: a rogue's bribe removes 10 points from the north gate.
    let a = c.sabotage("north".into(), U128(10_000 * COIN));
    assert_eq!(a.amount.0, 1_000);
    assert_eq!(c.get_kingdom().gates[0].defense, 4_000);
    assert_eq!(c.get_kingdom().pending_burn.0, 10_000 * COIN);
    // Steal odds at 40% defense for a rogue: 60% × 97% + 5% = 63.2%.
    let mut won = 0;
    for seed in 0..40u8 {
        testing_env!(ctx(accounts(1), seed, 2_000).build());
        let a = c.steal("north".into(), U128(100 * COIN));
        assert_eq!(a.chance_bps, 6_320);
        if a.won { won += 1; }
    }
    assert!(won > 15 && won < 38, "won {won}");
    let k = c.get_kingdom();
    assert_eq!(k.stats.steals, 40);
    assert_eq!(k.gates[0].steals_won + k.gates[0].steals_lost, 40);
    // Books: deposited 100k, pouch + bribes + stakes in play add up.
    let lost = 40 - won as u128;
    assert_eq!(c.get_balance(accounts(1)).0, 100_000 * COIN - 10_000 * COIN + won as u128 * 100 * COIN - lost * 100 * COIN);
    assert!(get_logs().iter().any(|l| l.contains("\"event\":\"steal\"")));
}

#[test]
fn outlaws_must_join_and_pay() {
    let tee = Tee::new(7);
    let mut c = kingdom(&tee);
    testing_env!(ctx(accounts(2), 1, 2_000).build());
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { c.steal("north".into(), U128(COIN)); })).is_err());
    testing_env!(ctx(accounts(1), 1, 2_000).build());
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { c.steal("west".into(), U128(COIN)); })).is_err());
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { c.steal("north".into(), U128(20_000 * COIN)); })).is_err(), "over 1% of the treasury");
}

#[test]
fn a_signed_valid_decision_fortifies_a_gate() {
    let tee = Tee::new(7);
    let mut c = kingdom(&tee);
    testing_env!(ctx(accounts(3), 2, 1_000 + ROUND).build());
    let (req, resp, text, sig) = sheriff_call(&c, &tee, "```json\n{\"action\":\"FORTIFY\",\"gate\":\"north\",\"budget\":20000,\"reason\":\"They keep hitting the north gate\"}\n```");
    let mv = c.sheriff_turn(req, resp, text, sig);
    assert!(mv.accepted, "{}", mv.verdict);
    let k = c.get_kingdom();
    assert_eq!(k.round, 2);
    assert_eq!(k.gates[0].defense, 7_000); // +20 points for 2% of the treasury
    assert_eq!(k.treasury.0, 980_000 * COIN);
    assert_eq!(k.stats.burned_by_sheriff.0, 20_000 * COIN);
    assert_eq!(c.get_log(None)[0].reason, "They keep hitting the north gate");
}

#[test]
fn the_crown_rejects_a_decision_over_budget() {
    let tee = Tee::new(7);
    let mut c = kingdom(&tee);
    testing_env!(ctx(accounts(3), 2, 1_000 + ROUND).build());
    let (req, resp, text, sig) = sheriff_call(&c, &tee, "{\"action\":\"HIRE_GUARDS\",\"budget\":90000,\"reason\":\"Crush them\"}");
    let mv = c.sheriff_turn(req, resp, text, sig);
    assert!(!mv.accepted);
    assert!(mv.verdict.contains("over the limit"), "{}", mv.verdict);
    let k = c.get_kingdom();
    assert_eq!(k.treasury.0, 1_000_000 * COIN);
    assert_eq!(k.stats.sheriff_rejected, 1);
    assert_eq!(k.round, 2);
}

#[test]
fn forged_or_stale_decisions_never_land() {
    let tee = Tee::new(7);
    let impostor = Tee::new(9);
    let mut c = kingdom(&tee);
    testing_env!(ctx(accounts(3), 2, 1_000 + ROUND).build());
    let try_turn = |c: &mut Contract, args: (String, String, String, String)| std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { c.sheriff_turn(args.0, args.1, args.2, args.3); })).is_err();
    // Signed by a key that is not on the allowlist.
    let forged = sheriff_call(&c, &impostor, "{\"action\":\"HOARD\"}");
    assert!(try_turn(&mut c, forged));
    // A tampered response (budget changed after signing).
    let (req, resp, text, sig) = sheriff_call(&c, &tee, "{\"action\":\"HOARD\"}");
    assert!(try_turn(&mut c, (req.clone(), resp.replace("HOARD", "FORTIFY"), text.clone(), sig.clone())));
    // A request made for another state (the digest no longer matches).
    let stale = req.replace("STATE_DIGEST=", "STATE_DIGEST=0");
    assert!(try_turn(&mut c, (stale, resp.clone(), text.clone(), sig.clone())));
    // Too early: the round is not over.
    testing_env!(ctx(accounts(3), 2, 1_000 + ROUND - 1).build());
    assert!(try_turn(&mut c, (req.clone(), resp.clone(), text.clone(), sig.clone())));
    // The genuine one lands.
    testing_env!(ctx(accounts(3), 2, 1_000 + ROUND).build());
    assert!(c.sheriff_turn(req, resp, text, sig).accepted);
}

#[test]
fn unreadable_answers_cost_the_sheriff_his_turn() {
    let tee = Tee::new(7);
    let mut c = kingdom(&tee);
    testing_env!(ctx(accounts(3), 2, 1_000 + ROUND).build());
    let (req, resp, text, sig) = sheriff_call(&c, &tee, "I refuse to answer in JSON.");
    let mv = c.sheriff_turn(req, resp, text, sig);
    assert!(!mv.accepted);
    assert_eq!(mv.action, "UNREADABLE");
}

#[test]
fn a_sleeping_sheriff_can_be_skipped() {
    let tee = Tee::new(7);
    let mut c = kingdom(&tee);
    testing_env!(ctx(accounts(4), 2, 1_000 + 2 * ROUND).build());
    c.skip_turn();
    assert_eq!(c.get_kingdom().round, 2);
    assert_eq!(c.get_log(None)[0].action, "SLEEP");
}
