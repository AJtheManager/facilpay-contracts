#![cfg(test)]
//! Tests for #756: pull-payment mandates with per-charge, per-period and total caps.

use soroban_sdk::{
    testutils::{Address as _, Events as _, Ledger as _},
    token, vec, Address, Env, Event, String,
};

use crate::{
    BasicError, Error, MandateCharged, MandateCreated, MandateRevoked, PaymentContract,
    PaymentContractClient, PaymentError, SubscriptionError, MAX_MANDATE_MEMO_SIZE,
};

const START: u64 = 1_000;
const DAY: u64 = 86_400;
const MAX_PER_CHARGE: i128 = 100;
const MAX_TOTAL: i128 = 1_000;
const MAX_PER_PERIOD: i128 = 250;
const EXPIRES_AT: u64 = START + 30 * DAY;

struct Fixture {
    env: Env,
    client: PaymentContractClient<'static>,
    merchant: Address,
    customer: Address,
    token: Address,
}

fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(START);

    let contract_id = env.register(PaymentContract, ());
    let client = PaymentContractClient::new(&env, &contract_id);
    client.initialize(&Address::generate(&env));

    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    let customer = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token).mint(&customer, &10_000);
    token::Client::new(&env, &token).approve(&customer, &contract_id, &10_000, &100_000);

    Fixture {
        merchant: Address::generate(&env),
        customer,
        env,
        client,
        token,
    }
}

fn create_mandate(f: &Fixture) -> u64 {
    f.client.create_mandate(
        &f.customer,
        &f.merchant,
        &f.token,
        &MAX_PER_CHARGE,
        &MAX_TOTAL,
        &DAY,
        &MAX_PER_PERIOD,
        &EXPIRES_AT,
    )
}

fn charge(f: &Fixture, mandate_id: u64, amount: i128) -> Result<i128, Error> {
    f.client
        .try_charge_mandate(
            &f.merchant,
            &mandate_id,
            &amount,
            &String::from_str(&f.env, "usage"),
        )
        .map(|r| r.unwrap())
        .map_err(|e| e.unwrap())
}

fn balance(f: &Fixture, who: &Address) -> i128 {
    token::Client::new(&f.env, &f.token).balance(who)
}

fn assert_last_event(f: &Fixture, event: &impl Event) {
    let all = f.env.events().all();
    assert_eq!(
        all.slice(all.len() - 1..),
        vec![
            &f.env,
            (
                f.client.address.clone(),
                event.topics(&f.env),
                event.data(&f.env)
            )
        ]
    );
}

#[test]
fn create_mandate_stores_terms_and_emits_event() {
    let f = setup();
    let id = create_mandate(&f);
    assert_eq!(id, 1);
    assert_last_event(
        &f,
        &MandateCreated {
            mandate_id: id,
            customer: f.customer.clone(),
            merchant: f.merchant.clone(),
            token: f.token.clone(),
            max_per_charge: MAX_PER_CHARGE,
            max_total: MAX_TOTAL,
            period_seconds: DAY,
            max_per_period: MAX_PER_PERIOD,
            expires_at: EXPIRES_AT,
        },
    );

    let m = f.client.get_mandate(&id).unwrap();
    assert_eq!(m.customer, f.customer);
    assert_eq!(m.merchant, f.merchant);
    assert_eq!(m.created_at, START);
    assert_eq!(m.period_start, START);
    assert_eq!(m.total_charged, 0);
    assert!(!m.revoked);
    assert_eq!(create_mandate(&f), 2);
}

#[test]
fn create_mandate_rejects_invalid_terms() {
    let f = setup();
    let cases: [(i128, i128, u64, i128, u64); 5] = [
        (0, MAX_TOTAL, DAY, MAX_PER_PERIOD, EXPIRES_AT),
        (MAX_PER_CHARGE, 0, DAY, MAX_PER_PERIOD, EXPIRES_AT),
        (MAX_PER_CHARGE, MAX_TOTAL, 0, MAX_PER_PERIOD, EXPIRES_AT),
        (MAX_PER_CHARGE, MAX_TOTAL, DAY, -1, EXPIRES_AT),
        (MAX_PER_CHARGE, MAX_TOTAL, DAY, MAX_PER_PERIOD, START),
    ];
    for (per_charge, total, period, per_period, expires_at) in cases {
        let result = f.client.try_create_mandate(
            &f.customer,
            &f.merchant,
            &f.token,
            &per_charge,
            &total,
            &period,
            &per_period,
            &expires_at,
        );
        assert_eq!(
            result,
            Err(Ok(Error::Subscription(SubscriptionError::InvalidMandate)))
        );
    }

    let self_mandate = f.client.try_create_mandate(
        &f.customer,
        &f.customer,
        &f.token,
        &MAX_PER_CHARGE,
        &MAX_TOTAL,
        &DAY,
        &MAX_PER_PERIOD,
        &EXPIRES_AT,
    );
    assert_eq!(
        self_mandate,
        Err(Ok(Error::Subscription(SubscriptionError::InvalidMandate)))
    );
}

#[test]
fn charge_pulls_funds_and_updates_counters() {
    let f = setup();
    let id = create_mandate(&f);

    assert_eq!(charge(&f, id, 60), Ok(60));
    assert_last_event(
        &f,
        &MandateCharged {
            mandate_id: id,
            customer: f.customer.clone(),
            merchant: f.merchant.clone(),
            amount: 60,
            memo: String::from_str(&f.env, "usage"),
            total_charged: 60,
            period_charged: 60,
        },
    );
    assert_eq!(charge(&f, id, 40), Ok(100));

    assert_eq!(balance(&f, &f.merchant), 100);
    assert_eq!(balance(&f, &f.customer), 9_900);
    let m = f.client.get_mandate(&id).unwrap();
    assert_eq!(m.total_charged, 100);
    assert_eq!(m.period_charged, 100);
    assert_eq!(m.charge_count, 2);
}

#[test]
fn charge_over_per_charge_cap_is_rejected() {
    let f = setup();
    let id = create_mandate(&f);
    assert_eq!(
        charge(&f, id, MAX_PER_CHARGE + 1),
        Err(Error::Subscription(
            SubscriptionError::MandatePerChargeExceeded
        ))
    );
    assert_eq!(charge(&f, id, MAX_PER_CHARGE), Ok(MAX_PER_CHARGE));
}

#[test]
fn charge_over_period_cap_is_rejected_until_next_period() {
    let f = setup();
    let id = create_mandate(&f);
    charge(&f, id, 100).unwrap();
    charge(&f, id, 100).unwrap();
    assert_eq!(
        charge(&f, id, 51),
        Err(Error::Subscription(
            SubscriptionError::MandatePeriodCapExceeded
        ))
    );
    assert_eq!(charge(&f, id, 50), Ok(250));

    // One second before the boundary the period is still full.
    f.env.ledger().set_timestamp(START + DAY - 1);
    assert_eq!(
        charge(&f, id, 1),
        Err(Error::Subscription(
            SubscriptionError::MandatePeriodCapExceeded
        ))
    );

    // At the boundary the counter resets.
    f.env.ledger().set_timestamp(START + DAY);
    assert_eq!(charge(&f, id, 100), Ok(350));
    let m = f.client.get_mandate(&id).unwrap();
    assert_eq!(m.period_start, START + DAY);
    assert_eq!(m.period_charged, 100);
}

#[test]
fn period_start_stays_aligned_after_skipped_periods() {
    let f = setup();
    let id = create_mandate(&f);
    charge(&f, id, 100).unwrap();

    // Skip ahead two and a half periods.
    f.env.ledger().set_timestamp(START + 2 * DAY + DAY / 2);
    charge(&f, id, 100).unwrap();
    let m = f.client.get_mandate(&id).unwrap();
    assert_eq!(m.period_start, START + 2 * DAY);
    assert_eq!(m.period_charged, 100);
}

#[test]
fn charge_over_total_cap_is_rejected() {
    let f = setup();
    let id = create_mandate(&f);
    for day in 0..4u64 {
        f.env.ledger().set_timestamp(START + day * DAY);
        charge(&f, id, 100).unwrap();
        charge(&f, id, 100).unwrap();
        charge(&f, id, 50).unwrap();
    }
    // 1_000 charged: the lifetime cap is reached even though a new period began.
    f.env.ledger().set_timestamp(START + 4 * DAY);
    assert_eq!(
        charge(&f, id, 1),
        Err(Error::Subscription(SubscriptionError::MandateTotalExceeded))
    );
    assert_eq!(f.client.get_mandate(&id).unwrap().total_charged, MAX_TOTAL);
}

#[test]
fn revoked_mandate_cannot_be_charged() {
    let f = setup();
    let id = create_mandate(&f);
    f.client.revoke_mandate(&f.customer, &id);
    assert_last_event(
        &f,
        &MandateRevoked {
            mandate_id: id,
            customer: f.customer.clone(),
            merchant: f.merchant.clone(),
        },
    );
    assert!(f.client.get_mandate(&id).unwrap().revoked);
    assert_eq!(
        charge(&f, id, 10),
        Err(Error::Subscription(SubscriptionError::MandateRevoked))
    );
    assert_eq!(
        f.client.try_revoke_mandate(&f.customer, &id),
        Err(Ok(Error::Subscription(SubscriptionError::MandateRevoked)))
    );
}

#[test]
fn only_the_customer_can_revoke() {
    let f = setup();
    let id = create_mandate(&f);
    assert_eq!(
        f.client.try_revoke_mandate(&f.merchant, &id),
        Err(Ok(Error::Basic(BasicError::Unauthorized)))
    );
}

#[test]
fn expired_mandate_cannot_be_charged() {
    let f = setup();
    let id = create_mandate(&f);
    f.env.ledger().set_timestamp(EXPIRES_AT - 1);
    assert_eq!(charge(&f, id, 10), Ok(10));
    f.env.ledger().set_timestamp(EXPIRES_AT);
    assert_eq!(
        charge(&f, id, 10),
        Err(Error::Subscription(SubscriptionError::MandateExpired))
    );
}

#[test]
fn only_the_mandate_merchant_can_charge() {
    let f = setup();
    let id = create_mandate(&f);
    let other = Address::generate(&f.env);
    let result = f
        .client
        .try_charge_mandate(&other, &id, &10, &String::from_str(&f.env, "usage"));
    assert_eq!(result, Err(Ok(Error::Basic(BasicError::Unauthorized))));
}

#[test]
fn charge_rejects_bad_input() {
    let f = setup();
    let id = create_mandate(&f);
    assert_eq!(
        charge(&f, id, 0),
        Err(Error::Basic(BasicError::InvalidAmount))
    );
    assert_eq!(
        charge(&f, 99, 10),
        Err(Error::Subscription(SubscriptionError::MandateNotFound))
    );

    let mut long = [b'a'; (MAX_MANDATE_MEMO_SIZE + 1) as usize];
    long[0] = b'm';
    let result =
        f.client
            .try_charge_mandate(&f.merchant, &id, &10, &String::from_bytes(&f.env, &long));
    assert_eq!(result, Err(Ok(Error::Basic(BasicError::NotesTooLarge))));
}

#[test]
fn failed_pull_leaves_mandate_unchanged() {
    let f = setup();
    let id = create_mandate(&f);
    token::Client::new(&f.env, &f.token).approve(&f.customer, &f.client.address, &5, &100_000);

    assert_eq!(
        charge(&f, id, 10),
        Err(Error::Payment(PaymentError::TransferFailed))
    );
    let m = f.client.get_mandate(&id).unwrap();
    assert_eq!(m.total_charged, 0);
    assert_eq!(m.charge_count, 0);
    assert_eq!(balance(&f, &f.merchant), 0);
}
