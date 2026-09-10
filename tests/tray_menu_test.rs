use byte::ops::manage::AccountListing;
use byte::store::metadata::AccountMeta;
use byte::tray::menu::{MenuEntry, MenuModel, MenuRow, menu_rows};

fn listing(uuid: &str, label: &str, org: Option<&str>, active: bool) -> AccountListing {
    AccountListing {
        meta: AccountMeta {
            uuid: uuid.to_string(),
            label: label.to_string(),
            email: Some(format!("{label}@example.com")),
            organization_name: org.map(str::to_string),
            subscription_type: Some("max".to_string()),
            account: serde_json::json!({"accountUuid": uuid}),
            user_id: Some("uid".to_string()),
            credential_schema: 1,
            added_at: "2026-01-01T00:00:00Z".to_string(),
            last_used_at: None,
            desktop_profile: None,
        },
        active,
    }
}

#[test]
fn an_empty_store_offers_only_add_and_quit() {
    let model = MenuModel::from_listing(&[]);
    assert_eq!(
        model.entries,
        vec![MenuEntry::AddAccount, MenuEntry::Separator, MenuEntry::Quit]
    );
}

#[test]
fn accounts_render_by_label_not_email() {
    // Users rename accounts; the menu must show what `byte list` shows.
    let model = MenuModel::from_listing(&[listing("u1", "work", Some("Indicio"), true)]);
    let MenuEntry::Account { label, detail, .. } = &model.entries[0] else {
        panic!(
            "first entry should be an account, got {:?}",
            model.entries[0]
        );
    };
    assert_eq!(label, "work");
    assert_eq!(detail.as_deref(), Some("Indicio"));
}

#[test]
fn exactly_the_active_account_is_marked() {
    // The active account sits in the middle -- neither first nor last --
    // so this fails against an implementation that derives `active` from
    // position (e.g. "mark whichever entry is last") instead of from
    // `listing.active`.
    let model = MenuModel::from_listing(&[
        listing("u1", "personal", None, false),
        listing("u2", "work", None, true),
        listing("u3", "side", None, false),
    ]);
    let marked: Vec<&str> = model
        .entries
        .iter()
        .filter_map(|e| match e {
            MenuEntry::Account {
                uuid, active: true, ..
            } => Some(uuid.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(marked, vec!["u2"], "only u2 should be marked active");
}

#[test]
fn account_order_from_the_listing_is_preserved() {
    // `beta` precedes `alpha` here so insertion order and alphabetical
    // order diverge (in both label and uuid) -- an implementation that
    // sorted instead of preserving input order would fail this.
    let model = MenuModel::from_listing(&[
        listing("u2", "beta", None, false),
        listing("u1", "alpha", None, false),
    ]);
    let uuids: Vec<&str> = model
        .entries
        .iter()
        .filter_map(|e| match e {
            MenuEntry::Account { uuid, .. } => Some(uuid.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(uuids, vec!["u2", "u1"]);
}

#[test]
fn a_separator_divides_accounts_from_the_actions() {
    let model = MenuModel::from_listing(&[listing("u1", "work", None, true)]);
    assert_eq!(
        model.entries,
        vec![
            MenuEntry::Account {
                uuid: "u1".to_string(),
                label: "work".to_string(),
                detail: None,
                active: true,
            },
            MenuEntry::Separator,
            MenuEntry::AddAccount,
            MenuEntry::Separator,
            MenuEntry::Quit,
        ]
    );
}

#[test]
fn an_account_with_no_organization_has_no_detail_line() {
    let model = MenuModel::from_listing(&[listing("u1", "solo", None, false)]);
    let MenuEntry::Account { detail, .. } = &model.entries[0] else {
        panic!("expected an account entry");
    };
    assert_eq!(*detail, None);
}

// The brief's tests above each look at accounts or the footer in isolation:
// `account_order_from_the_listing_is_preserved` filters down to just the
// `Account` entries and ignores everything else, so it would not notice a
// footer that goes missing or malforms once there is more than one account
// ahead of it. This test pins the exact tail shape with two accounts
// present, so an implementation that only gets the footer right for the
// single-account case (e.g. an off-by-one on `entries.len()`) cannot pass.
#[test]
fn the_footer_keeps_its_shape_with_more_than_one_account() {
    let model = MenuModel::from_listing(&[
        listing("u1", "alpha", None, false),
        listing("u2", "beta", None, false),
    ]);
    assert_eq!(model.entries.len(), 6);
    assert_eq!(
        &model.entries[2..],
        &[
            MenuEntry::Separator,
            MenuEntry::AddAccount,
            MenuEntry::Separator,
            MenuEntry::Quit,
        ]
    );
}

// The index-parallelism invariant, which previously lived only in a comment
// inside `App::rebuild` -- a function needing a real TrayIcon, and so having
// no coverage at all. Deleting the separator's row there would shift every
// later index: with one account stored, a click on "Quit" would resolve to
// "Add account…" and silently spawn a terminal instead of quitting.

#[test]
fn every_entry_renders_exactly_one_row() {
    let model = MenuModel::from_listing(&[
        listing("u1", "personal", None, false),
        listing("u2", "work", Some("Indicio"), true),
    ]);
    let rows = menu_rows(&model);
    assert_eq!(
        rows.len(),
        model.entries.len(),
        "rows and entries are looked up by shared index and must stay parallel"
    );
}

#[test]
fn separators_occupy_a_row_of_their_own() {
    // The specific deletion that breaks the invariant: dropping separator
    // rows. Asserting the positions of the separators pins it directly.
    let model = MenuModel::from_listing(&[listing("u1", "work", None, true)]);
    let rows = menu_rows(&model);
    for (index, entry) in model.entries.iter().enumerate() {
        let is_separator = matches!(entry, MenuEntry::Separator);
        assert_eq!(
            matches!(rows[index], MenuRow::Separator),
            is_separator,
            "row {index} disagrees with its entry about being a separator"
        );
    }
}

#[test]
fn exactly_the_active_accounts_row_is_marked() {
    // Identity, not position: the active account is in the middle.
    let model = MenuModel::from_listing(&[
        listing("u1", "personal", None, false),
        listing("u2", "work", None, true),
        listing("u3", "other", None, false),
    ]);
    let marked: Vec<String> = menu_rows(&model)
        .into_iter()
        .filter_map(|row| match row {
            MenuRow::Item(text) if text.starts_with('●') => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(
        marked.len(),
        1,
        "exactly one row should be marked active: {marked:?}"
    );
    assert!(
        marked[0].contains("work"),
        "the marked row must be the active account, got {}",
        marked[0]
    );
}

#[test]
fn the_add_and_quit_rows_render_their_own_text() {
    let rows = menu_rows(&MenuModel::from_listing(&[]));
    assert!(
        rows.contains(&MenuRow::Item("Add account…".to_string())),
        "{rows:?}"
    );
    assert!(
        rows.contains(&MenuRow::Item("Quit".to_string())),
        "{rows:?}"
    );
}
