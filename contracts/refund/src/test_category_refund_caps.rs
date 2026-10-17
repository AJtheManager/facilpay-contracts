#![cfg(test)]

//! Category-specific refund percentage caps (#807).

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    vec, Address, Env, String,
};

const DAY: u64 = 86_400;
const NOW: u64 = 100 * DAY;
const ORIGINAL: i128 = 1_000;

struct Setup<'a> {
    env: Env,
    client: RefundContractClient<'a>,
    merchant: Address,
    customer: Address,
    token: Address,
}

/// Merchant policy allows `tier_bps` for 30 days after purchase.
fn setup<'a>(tier_bps: u32) -> Setup<'a> {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(NOW);

    let admin = Address::generate(&env);
    let merchant = Address::generate(&env);
    let customer = Address::generate(&env);
    let token = Address::generate(&env);

    let contract_id = env.register(RefundContract, ());
    let client = RefundContractClient::new(&env, &contract_id);
    client.initialize(&admin);
    client.set_fraud_config(
        &admin,
        &FraudConfig {
            max_refund_rate_bps: 10000,
            min_transactions_for_check: 1000,
            enabled: false,
        },
    );
    client.set_refund_policy(
        &merchant,
        &vec![
            &env,
            RefundTier {
                days_from_purchase: 30,
                max_refund_bps: tier_bps,
            },
        ],
    );

    Setup {
        env,
        client,
        merchant,
        customer,
        token,
    }
}

fn try_request(
    s: &Setup,
    payment_id: u64,
    amount: i128,
) -> Result<Result<u64, soroban_sdk::Error>, Result<Error, soroban_sdk::InvokeError>> {
    s.client.try_request_refund(
        &s.merchant,
        &payment_id,
        &s.customer,
        &amount,
        &ORIGINAL,
        &s.token,
        &String::from_str(&s.env, "reason"),
        &RefundReasonCode::Other,
        &(NOW - DAY),
    )
}

fn exceeds_policy(
) -> Result<Result<u64, soroban_sdk::Error>, Result<Error, soroban_sdk::InvokeError>> {
    Err(Ok(Error::Core(CoreError::RefundExceedsPolicy)))
}

#[test]
fn test_set_and_get_category_refund_cap() {
    let s = setup(10000);
    assert_eq!(
        s.client
            .get_category_refund_cap(&s.merchant, &PaymentCategory::DigitalGoods),
        None
    );
    s.client
        .set_category_refund_cap(&s.merchant, &PaymentCategory::DigitalGoods, &2500);
    assert_eq!(
        s.client
            .get_category_refund_cap(&s.merchant, &PaymentCategory::DigitalGoods),
        Some(2500)
    );
    assert_eq!(
        s.client
            .get_category_refund_cap(&s.merchant, &PaymentCategory::Service),
        None
    );
}

#[test]
fn test_category_refund_cap_rejects_bps_above_10000() {
    let s = setup(10000);
    assert_eq!(
        s.client
            .try_set_category_refund_cap(&s.merchant, &PaymentCategory::Other, &10001),
        Err(Ok(Error::Ext(ExtError::InvalidCategoryRefundCap)))
    );
}

#[test]
#[should_panic]
fn test_category_refund_cap_requires_merchant_auth() {
    let env = Env::default();
    let contract_id = env.register(RefundContract, ());
    let client = RefundContractClient::new(&env, &contract_id);
    let merchant = Address::generate(&env);
    client.set_category_refund_cap(&merchant, &PaymentCategory::Other, &100);
}

#[test]
fn test_category_cap_lower_than_tier_wins() {
    let s = setup(10000);
    s.client
        .set_category_refund_cap(&s.merchant, &PaymentCategory::DigitalGoods, &2000);
    s.client
        .tag_payment_category(&s.merchant, &1, &PaymentCategory::DigitalGoods);

    assert_eq!(try_request(&s, 1, 201), exceeds_policy());
    assert!(try_request(&s, 1, 200).is_ok());
}

#[test]
fn test_tier_lower_than_category_cap_wins() {
    let s = setup(1000);
    s.client
        .set_category_refund_cap(&s.merchant, &PaymentCategory::DigitalGoods, &5000);
    s.client
        .tag_payment_category(&s.merchant, &1, &PaymentCategory::DigitalGoods);

    assert_eq!(try_request(&s, 1, 101), exceeds_policy());
    assert!(try_request(&s, 1, 100).is_ok());
}

#[test]
fn test_cap_only_applies_to_tagged_category() {
    let s = setup(10000);
    s.client
        .set_category_refund_cap(&s.merchant, &PaymentCategory::DigitalGoods, &2000);
    s.client
        .tag_payment_category(&s.merchant, &2, &PaymentCategory::PhysicalGoods);

    // Untagged payment and a payment in another category use the tier only.
    assert!(try_request(&s, 1, 900).is_ok());
    assert!(try_request(&s, 2, 900).is_ok());
}

#[test]
fn test_category_caps_are_per_merchant() {
    let s = setup(10000);
    let other = Address::generate(&s.env);
    s.client
        .set_category_refund_cap(&other, &PaymentCategory::DigitalGoods, &100);
    s.client
        .tag_payment_category(&s.merchant, &1, &PaymentCategory::DigitalGoods);

    assert!(try_request(&s, 1, 900).is_ok());
}
