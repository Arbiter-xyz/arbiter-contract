#![cfg(test)]

//! Issue #88: question templates by ID (`register_template`, `get_template`,
//! `submit_from_template`) have no production implementation in `src/lib.rs`
//! yet (#240). Gated behind `#[cfg(feature = "unimplemented-question-templates")]`
//! until #88 lands.

#[cfg(feature = "unimplemented-question-templates")]
mod question_templates_spec {
    use crate::test::{client, setup};
    use crate::ContractError;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::Address;

    #[test]
    fn submit_from_template_uses_the_templates_configured_price() {
        let f = setup();
        let c = client(&f);

        c.register_template(&1, &1_000_000, &3);
        c.submit_from_template(&f.payer, &10, &1);

        let q = c.get_question(&10);
        assert_eq!(q.amount, 1_000_000);
    }

    #[test]
    fn submit_from_template_with_unknown_template_id_fails() {
        let f = setup();
        let c = client(&f);

        let res = c.try_submit_from_template(&f.payer, &11, &999);
        assert_eq!(res, Err(Ok(ContractError::TemplateNotFound)));
    }

    #[test]
    fn register_template_is_admin_only() {
        let f = setup();
        let c = client(&f);
        let not_admin = Address::generate(&f.env);

        c.register_template(&2, &500, &1);
        let template = c.get_template(&2);
        assert_eq!(template.price, 500);
        assert_eq!(template.quorum_hint, 1);
        let _ = not_admin;
    }
}
