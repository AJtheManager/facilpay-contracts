#![cfg(test)]

use super::*;
use soroban_sdk::testutils::{Address as _, Events, Ledger};
use soroban_sdk::{token, vec, Address, Env, Symbol, TryFromVal};

const EXPIRY: u64 = 1_000;
const RESERVE: i128 = 10_000;

struct Setup<'a> {
    env: Env,
    client: RefundContractClient<'a>,
    contract_id: Address,
    merchant: Address,
    token: Address,
}

fn setup<'a>() -> Setup<'a> {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000);
    let contract_id = env.register(RefundContract, ());
    let client = RefundContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let merchant = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token).mint(&merchant, &1_000_000);

    Setup {
        env,
        client,
        contract_id,
        merchant,
        token,
    }
}

fn fund_reserve(s: &Setup, amount: i128) {
    s.client
        .deposit_refund_reserve(&s.merchant, &s.token, &amount);
}

fn balance_of(s: &Setup, who: &Address) -> i128 {
    token::Client::new(&s.env, &s.token).balance(who)
}

fn event_count(env: &Env, name: &str) -> usize {
    let expected = Symbol::new(env, name);
    env.events()
        .all()
        .iter()
        .filter(|(_, topics, _)| {
            topics
                .get(0)
                .and_then(|t| Symbol::try_from_val(env, &t).ok())
                .is_some_and(|s| s == expected)
        })
        .count()
}

#[test]
fn issues_vouchers_to_every_recipient_and_debits_reserve() {
    let s = setup();
    fund_reserve(&s, RESERVE);
    let alice = Address::generate(&s.env);
    let bob = Address::generate(&s.env);

    let ids = s.client.issue_promotional_vouchers(
        &s.merchant,
        &vec![&s.env, (alice.clone(), 300), (bob.clone(), 700)],
        &s.token,
        &EXPIRY,
    );

    assert_eq!(ids.len(), 2);
    assert_eq!(event_count(&s.env, "promotional_voucher_issued"), 2);
    assert_eq!(event_count(&s.env, "promotional_vouchers_issued"), 1);
    assert_eq!(
        s.client.get_refund_reserve(&s.merchant, &s.token),
        RESERVE - 1_000
    );

    let first = s.client.get_voucher(&ids.get(0).unwrap()).unwrap();
    assert_eq!(first.customer, alice);
    assert_eq!(first.merchant, s.merchant);
    assert_eq!(first.amount, 300);
    assert_eq!(first.refund_id, 0);
    assert_eq!(first.expires_at, 1_000 + EXPIRY);
    assert!(!first.redeemed);

    let second = s.client.get_voucher(&ids.get(1).unwrap()).unwrap();
    assert_eq!(second.customer, bob);
    assert_eq!(second.amount, 700);
    assert_eq!(s.client.get_customer_vouchers(&bob).len(), 1);
}

#[test]
fn promotional_voucher_can_be_redeemed() {
    let s = setup();
    fund_reserve(&s, RESERVE);
    let alice = Address::generate(&s.env);
    let ids = s.client.issue_promotional_vouchers(
        &s.merchant,
        &vec![&s.env, (alice.clone(), 400)],
        &s.token,
        &EXPIRY,
    );

    s.client
        .redeem_refund_voucher(&alice, &ids.get(0).unwrap(), &0);
    assert_eq!(balance_of(&s, &alice), 400);
    assert_eq!(balance_of(&s, &s.contract_id), RESERVE - 400);
}

#[test]
fn voucher_ids_continue_from_existing_counter() {
    let s = setup();
    fund_reserve(&s, RESERVE);
    let alice = Address::generate(&s.env);
    let first = s.client.issue_promotional_vouchers(
        &s.merchant,
        &vec![&s.env, (alice.clone(), 100)],
        &s.token,
        &EXPIRY,
    );
    let second = s.client.issue_promotional_vouchers(
        &s.merchant,
        &vec![&s.env, (alice.clone(), 100), (alice.clone(), 100)],
        &s.token,
        &EXPIRY,
    );
    assert_eq!(first, vec![&s.env, 1u64]);
    assert_eq!(second, vec![&s.env, 2u64, 3u64]);
}

#[test]
fn insufficient_reserve_rejects_batch_before_issuing_anything() {
    let s = setup();
    fund_reserve(&s, 500);
    let alice = Address::generate(&s.env);
    let bob = Address::generate(&s.env);

    let result = s.client.try_issue_promotional_vouchers(
        &s.merchant,
        &vec![&s.env, (alice.clone(), 300), (bob.clone(), 300)],
        &s.token,
        &EXPIRY,
    );
    assert_eq!(
        result,
        Err(Ok(Error::Ext(ExtError::InsufficientRefundReserve)))
    );
    assert_eq!(s.client.get_refund_reserve(&s.merchant, &s.token), 500);
    assert_eq!(s.client.get_customer_vouchers(&alice).len(), 0);
    assert_eq!(s.client.get_customer_vouchers(&bob).len(), 0);
    assert!(s.client.get_voucher(&1).is_none());
}

#[test]
fn merchant_without_reserve_is_rejected() {
    let s = setup();
    let alice = Address::generate(&s.env);
    let result = s.client.try_issue_promotional_vouchers(
        &s.merchant,
        &vec![&s.env, (alice, 1)],
        &s.token,
        &EXPIRY,
    );
    assert_eq!(
        result,
        Err(Ok(Error::Ext(ExtError::InsufficientRefundReserve)))
    );
}

#[test]
fn batch_at_cap_succeeds_and_over_cap_is_rejected() {
    let s = setup();
    fund_reserve(&s, RESERVE);

    let mut batch = Vec::new(&s.env);
    for _ in 0..RefundContract::MAX_PROMOTIONAL_BATCH {
        batch.push_back((Address::generate(&s.env), 1i128));
    }
    let ids = s
        .client
        .issue_promotional_vouchers(&s.merchant, &batch, &s.token, &EXPIRY);
    assert_eq!(ids.len(), RefundContract::MAX_PROMOTIONAL_BATCH);

    batch.push_back((Address::generate(&s.env), 1i128));
    let result = s
        .client
        .try_issue_promotional_vouchers(&s.merchant, &batch, &s.token, &EXPIRY);
    assert_eq!(
        result,
        Err(Ok(Error::Ext(ExtError::PromotionalBatchTooLarge)))
    );
}

#[test]
fn empty_batch_is_rejected() {
    let s = setup();
    fund_reserve(&s, RESERVE);
    let result =
        s.client
            .try_issue_promotional_vouchers(&s.merchant, &Vec::new(&s.env), &s.token, &EXPIRY);
    assert_eq!(result, Err(Ok(Error::Ext(ExtError::EmptyPromotionalBatch))));
}

#[test]
fn non_positive_amount_is_rejected() {
    let s = setup();
    fund_reserve(&s, RESERVE);
    let alice = Address::generate(&s.env);
    let bob = Address::generate(&s.env);
    let result = s.client.try_issue_promotional_vouchers(
        &s.merchant,
        &vec![&s.env, (alice, 100), (bob, 0)],
        &s.token,
        &EXPIRY,
    );
    assert_eq!(result, Err(Ok(Error::Core(CoreError::InvalidAmount))));
    assert_eq!(s.client.get_refund_reserve(&s.merchant, &s.token), RESERVE);
}

#[test]
fn merchant_cannot_issue_voucher_to_itself() {
    let s = setup();
    fund_reserve(&s, RESERVE);
    let result = s.client.try_issue_promotional_vouchers(
        &s.merchant,
        &vec![&s.env, (s.merchant.clone(), 100)],
        &s.token,
        &EXPIRY,
    );
    assert_eq!(
        result,
        Err(Ok(Error::Ext(ExtError::InvalidVoucherRecipient)))
    );
}

#[test]
fn vouchers_snapshot_merchant_transferability() {
    let s = setup();
    fund_reserve(&s, RESERVE);
    s.client.set_vouchers_transferable(&s.merchant, &false);
    let alice = Address::generate(&s.env);
    let ids = s.client.issue_promotional_vouchers(
        &s.merchant,
        &vec![&s.env, (alice, 100)],
        &s.token,
        &EXPIRY,
    );
    assert!(!s.client.is_voucher_transferable(&ids.get(0).unwrap()));
}
