#![cfg(test)]

//! Merchant refund statement for a period (#810).

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, Env, String,
};

struct Setup<'a> {
    env: Env,
    client: RefundContractClient<'a>,
    admin: Address,
    merchant: Address,
    customer: Address,
}

fn setup<'a>() -> Setup<'a> {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(RefundContract, ());
    let client = RefundContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    client.initialize(&admin);
    client.set_fraud_config(
        &admin,
        &FraudConfig {
            max_refund_rate_bps: 10000,
            min_transactions_for_check: 1000,
            enabled: false,
        },
    );
    let merchant = Address::generate(&env);
    let customer = Address::generate(&env);
    Setup {
        env,
        client,
        admin,
        merchant,
        customer,
    }
}

/// Requests a refund at ledger time `at`.
fn request_at(s: &Setup, at: u64, payment_id: u64, amount: i128, token: &Address) -> u64 {
    s.env.ledger().set_timestamp(at);
    s.client.request_refund(
        &s.merchant,
        &payment_id,
        &s.customer,
        &amount,
        &amount,
        token,
        &String::from_str(&s.env, "statement"),
        &RefundReasonCode::Other,
        &0_u64,
    )
}

fn status_entry(statement: &MerchantRefundStatement, status: RefundStatus) -> StatusRefundTotals {
    statement
        .by_status
        .iter()
        .find(|e| e.status == status)
        .unwrap()
}

#[test]
fn test_statement_matches_summing_individual_refunds() {
    let s = setup();
    let token_a = Address::generate(&s.env);
    let token_b = Address::generate(&s.env);

    let _before = request_at(&s, 50, 1, 999, &token_a);
    let r1 = request_at(&s, 100, 2, 100, &token_a);
    let r2 = request_at(&s, 150, 3, 250, &token_b);
    let r3 = request_at(&s, 200, 4, 40, &token_a);
    let _after = request_at(&s, 201, 5, 999, &token_b);

    s.client.approve_refund(&s.admin, &r2);
    s.client
        .reject_refund(&s.admin, &r3, &String::from_str(&s.env, "no"));

    let statement = s
        .client
        .get_merchant_refund_statement(&s.merchant, &100, &200);

    // Cross-check against the individual refunds in the range.
    let in_range = s
        .client
        .get_merchant_refunds_in_range(&s.merchant, &100, &200, &100, &0);
    assert_eq!(in_range.len(), 3);
    let sum: i128 = in_range.iter().map(|r| r.amount).sum();
    assert_eq!(statement.refund_count, 3);
    assert_eq!(statement.total_amount, sum);
    assert_eq!(sum, 390);

    for entry in statement.by_status.iter() {
        let count = in_range.iter().filter(|r| r.status == entry.status).count() as u64;
        let total: i128 = in_range
            .iter()
            .filter(|r| r.status == entry.status)
            .map(|r| r.amount)
            .sum();
        assert_eq!(entry.count, count);
        assert_eq!(entry.total_amount, total);
    }
    assert_eq!(statement.by_status.len(), 6);
    assert_eq!(status_entry(&statement, RefundStatus::Requested).count, 1);
    assert_eq!(
        status_entry(&statement, RefundStatus::Approved).total_amount,
        250
    );
    // A rejection sits in PendingAppeal until its appeal window closes.
    assert_eq!(
        status_entry(&statement, RefundStatus::PendingAppeal).total_amount,
        40
    );
    assert_eq!(status_entry(&statement, RefundStatus::Processed).count, 0);

    assert_eq!(statement.by_token.len(), 2);
    let a = statement.by_token.get(0).unwrap();
    assert_eq!((a.token, a.count, a.total_amount), (token_a, 2, 140));
    let b = statement.by_token.get(1).unwrap();
    assert_eq!((b.token, b.count, b.total_amount), (token_b, 1, 250));

    let _ = r1;
}

#[test]
fn test_statement_empty_range_and_unknown_merchant() {
    let s = setup();
    let token = Address::generate(&s.env);
    request_at(&s, 100, 1, 10, &token);

    let empty = s
        .client
        .get_merchant_refund_statement(&s.merchant, &101, &500);
    assert_eq!(empty.refund_count, 0);
    assert_eq!(empty.total_amount, 0);
    assert_eq!(empty.by_token.len(), 0);

    let stranger = Address::generate(&s.env);
    let none = s
        .client
        .get_merchant_refund_statement(&stranger, &0, &u64::MAX);
    assert_eq!(none.refund_count, 0);
}

#[test]
fn test_statement_rejects_inverted_range() {
    let s = setup();
    assert_eq!(
        s.client
            .try_get_merchant_refund_statement(&s.merchant, &10, &9),
        Err(Ok(Error::Ext(ExtError::InvalidDateRange)))
    );
}

#[test]
fn test_statement_range_is_capped() {
    let s = setup();
    let token = Address::generate(&s.env);
    let max = RefundContract::MAX_STATEMENT_REFUNDS;
    for i in 0..=max {
        request_at(&s, 1_000 + i, i + 1, 1, &token);
    }

    assert_eq!(
        s.client
            .try_get_merchant_refund_statement(&s.merchant, &0, &u64::MAX),
        Err(Ok(Error::Ext(ExtError::StatementRangeTooLarge)))
    );

    // Exactly the cap is allowed, and the binary search skips earlier refunds.
    let capped = s
        .client
        .get_merchant_refund_statement(&s.merchant, &1_001, &u64::MAX);
    assert_eq!(capped.refund_count, max);
    assert_eq!(capped.total_amount, max as i128);
}
