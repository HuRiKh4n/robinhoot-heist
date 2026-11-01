//! tGOLD: a free test coin with a faucet, so anyone can play The Heist on testnet.
//! Standard NEP-141 + NEP-148, plus `faucet()`: up to 10,000 tGOLD per call, registration included.

use near_contract_standards::fungible_token::metadata::{FungibleTokenMetadata, FungibleTokenMetadataProvider, FT_METADATA_SPEC};
use near_contract_standards::fungible_token::{FungibleToken, FungibleTokenCore, FungibleTokenResolver};
use near_contract_standards::storage_management::{StorageBalance, StorageBalanceBounds, StorageManagement};
use near_sdk::borsh::BorshSerialize;
use near_sdk::json_types::U128;
use near_sdk::{env, log, near, require, AccountId, BorshStorageKey, NearToken, PanicOnDefault, PromiseOrValue};

pub const DECIMALS: u8 = 18;
pub const ONE: u128 = 1_000_000_000_000_000_000;
pub const FAUCET_AMOUNT: u128 = 10_000 * ONE;

#[derive(BorshSerialize, BorshStorageKey)]
#[borsh(crate = "near_sdk::borsh")]
enum Key {
    Token,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    token: FungibleToken,
}

#[near]
impl Contract {
    #[init]
    pub fn new(owner: AccountId, supply: U128) -> Self {
        let mut token = FungibleToken::new(Key::Token);
        token.internal_register_account(&owner);
        token.internal_deposit(&owner, supply.0);
        Self { token }
    }

    /// Free test coins: registers the caller (or `account`) and gives 10,000 tGOLD.
    pub fn faucet(&mut self, account: Option<AccountId>) -> U128 {
        let account = account.unwrap_or_else(env::predecessor_account_id);
        if !self.token.accounts.contains_key(&account) {
            self.token.internal_register_account(&account);
        }
        self.token.internal_deposit(&account, FAUCET_AMOUNT);
        log!("Faucet: {} tGOLD to {}", FAUCET_AMOUNT / ONE, account);
        U128(FAUCET_AMOUNT)
    }
}

#[near]
impl FungibleTokenCore for Contract {
    #[payable]
    fn ft_transfer(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>) {
        self.token.ft_transfer(receiver_id, amount, memo)
    }

    #[payable]
    fn ft_transfer_call(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>, msg: String) -> PromiseOrValue<U128> {
        self.token.ft_transfer_call(receiver_id, amount, memo, msg)
    }

    fn ft_total_supply(&self) -> U128 {
        self.token.ft_total_supply()
    }

    fn ft_balance_of(&self, account_id: AccountId) -> U128 {
        self.token.ft_balance_of(account_id)
    }
}

#[near]
impl FungibleTokenResolver for Contract {
    #[private]
    fn ft_resolve_transfer(&mut self, sender_id: AccountId, receiver_id: AccountId, amount: U128) -> U128 {
        let (used, _burned) = self.token.internal_ft_resolve_transfer(&sender_id, receiver_id, amount);
        used.into()
    }
}

#[near]
impl StorageManagement for Contract {
    #[payable]
    fn storage_deposit(&mut self, account_id: Option<AccountId>, registration_only: Option<bool>) -> StorageBalance {
        self.token.storage_deposit(account_id, registration_only)
    }

    #[payable]
    fn storage_withdraw(&mut self, amount: Option<NearToken>) -> StorageBalance {
        self.token.storage_withdraw(amount)
    }

    #[payable]
    fn storage_unregister(&mut self, force: Option<bool>) -> bool {
        self.token.internal_storage_unregister(force).is_some()
    }

    fn storage_balance_bounds(&self) -> StorageBalanceBounds {
        self.token.storage_balance_bounds()
    }

    fn storage_balance_of(&self, account_id: AccountId) -> Option<StorageBalance> {
        self.token.storage_balance_of(account_id)
    }
}

#[near]
impl FungibleTokenMetadataProvider for Contract {
    fn ft_metadata(&self) -> FungibleTokenMetadata {
        FungibleTokenMetadata {
            spec: FT_METADATA_SPEC.to_string(),
            name: "Test Gold".to_string(),
            symbol: "tGOLD".to_string(),
            icon: Some("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 32 32'%3E%3Ccircle cx='16' cy='16' r='14' fill='%23d6b26a'/%3E%3Ctext x='16' y='21' font-size='14' text-anchor='middle' fill='%23402a08'%3EG%3C/text%3E%3C/svg%3E".to_string()),
            reference: None,
            reference_hash: None,
            decimals: DECIMALS,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use near_sdk::test_utils::{accounts, VMContextBuilder};
    use near_sdk::testing_env;

    #[test]
    fn faucet_registers_and_pays() {
        let mut b = VMContextBuilder::new();
        testing_env!(b.predecessor_account_id(accounts(0)).build());
        let mut c = Contract::new(accounts(0), U128(1_000_000 * ONE));
        testing_env!(b.predecessor_account_id(accounts(1)).build());
        c.faucet(None);
        c.faucet(None);
        assert_eq!(c.ft_balance_of(accounts(1)).0, 2 * FAUCET_AMOUNT);
        require!(c.ft_total_supply().0 == 1_000_000 * ONE + 2 * FAUCET_AMOUNT);
    }
}
