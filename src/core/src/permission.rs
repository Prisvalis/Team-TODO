//! Who may do what to an issue. See the permission table in docs/rule/README.md.
//!
//! `can_view` and `VISIBLE_TO` express the same rule for single reads and list
//! queries; change them together.

use crate::domain::{Issue, Label, Series, Status, User, Visibility};

/// SQL predicate for issues visible to the viewer; bind the viewer id twice.
pub const VISIBLE_TO: &str = "(i.visibility = 'public' OR i.issuer_id = ? OR i.assignee_id = ?)";

fn is_issuer(issue: &Issue, viewer: &str) -> bool {
    issue.issuer_id == viewer
}

fn is_assignee(issue: &Issue, viewer: &str) -> bool {
    issue.assignee_id.as_deref() == Some(viewer)
}

pub fn can_view(issue: &Issue, viewer: &str) -> bool {
    issue.visibility == Visibility::Public || is_issuer(issue, viewer) || is_assignee(issue, viewer)
}

/// Editing covers every field and deleting. Administrators take over issues whose
/// issuer was deactivated, so that work is not frozen forever.
pub fn can_manage(issue: &Issue, viewer: &User) -> bool {
    is_issuer(issue, &viewer.id) || (viewer.is_admin && !issue.issuer_active)
}

/// Administrators may also remove any issue they can see, e.g. duplicates or spam.
pub fn can_delete(issue: &Issue, viewer: &User) -> bool {
    can_manage(issue, viewer) || viewer.is_admin
}

pub fn can_change_status(issue: &Issue, viewer: &User) -> bool {
    is_assignee(issue, &viewer.id) || can_manage(issue, viewer)
}

pub fn can_claim(issue: &Issue, viewer: &str) -> bool {
    issue.status != Status::Done && issue.assignee_id.is_none() && can_view(issue, viewer)
}

/// Whether the viewer still sees the issue after giving up its assignment.
pub fn can_view_after_release(issue: &Issue, viewer: &str) -> bool {
    issue.visibility == Visibility::Public || is_issuer(issue, viewer)
}

/// A series follows the rules of the issues it generates.
pub fn can_view_series(series: &Series, viewer: &str) -> bool {
    series.visibility == Visibility::Public
        || series.issuer_id == viewer
        || series.assignee_id.as_deref() == Some(viewer)
}

pub fn can_manage_series(series: &Series, viewer: &User) -> bool {
    series.issuer_id == viewer.id || (viewer.is_admin && !series.issuer_active)
}

/// System labels belong to administrators; custom ones also to their creator.
pub fn can_delete_label(label: &Label, viewer: &User) -> bool {
    viewer.is_admin || (!label.is_system && label.created_by.as_deref() == Some(viewer.id.as_str()))
}

pub fn can_release(issue: &Issue, viewer: &str) -> bool {
    is_assignee(issue, viewer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(id: &str, is_admin: bool) -> User {
        User {
            id: id.into(),
            username: id.into(),
            display_name: id.into(),
            email: None,
            is_admin,
            is_active: true,
            must_change_password: false,
        }
    }

    fn issue(visibility: Visibility, assignee: Option<&str>) -> Issue {
        Issue {
            id: "i1".into(),
            title: "t".into(),
            body: String::new(),
            status: Status::Open,
            visibility,
            issuer_id: "alice".into(),
            issuer_name: "Alice".into(),
            issuer_active: true,
            assignee_id: assignee.map(Into::into),
            assignee_name: None,
            assignee_active: true,
            start_at: None,
            due_at: None,
            created_at: 0,
            updated_at: 0,
            labels: Vec::new(),
            series_id: None,
        }
    }

    #[test]
    fn private_without_assignee_is_issuer_only() {
        let i = issue(Visibility::Private, None);
        assert!(can_view(&i, "alice"));
        assert!(!can_view(&i, "bob"));
    }

    #[test]
    fn private_with_assignee_is_issuer_and_assignee() {
        let i = issue(Visibility::Private, Some("bob"));
        assert!(can_view(&i, "alice"));
        assert!(can_view(&i, "bob"));
        assert!(!can_view(&i, "carol"));
    }

    #[test]
    fn public_is_visible_to_everyone() {
        let i = issue(Visibility::Public, Some("bob"));
        assert!(can_view(&i, "carol"));
    }

    #[test]
    fn assignee_can_change_status_and_release_only() {
        let i = issue(Visibility::Public, Some("bob"));
        assert!(can_change_status(&i, &user("bob", false)));
        assert!(can_release(&i, "bob"));
        assert!(!can_manage(&i, &user("bob", false)));
    }

    #[test]
    fn issuer_can_manage_and_change_status() {
        let i = issue(Visibility::Private, Some("bob"));
        assert!(can_manage(&i, &user("alice", false)));
        assert!(can_change_status(&i, &user("alice", false)));
        assert!(!can_release(&i, "alice"));
    }

    #[test]
    fn other_users_cannot_modify_public_issue() {
        let i = issue(Visibility::Public, Some("bob"));
        assert!(!can_manage(&i, &user("carol", false)));
        assert!(
            !can_manage(&i, &user("carol", true)),
            "admins cannot edit others' issues"
        );
        assert!(can_delete(&i, &user("carol", true)), "but may delete them");
        assert!(!can_delete(&i, &user("carol", false)));
        assert!(!can_change_status(&i, &user("carol", false)));
        assert!(!can_claim(&i, "carol"));
    }

    #[test]
    fn only_unassigned_visible_issues_can_be_claimed() {
        assert!(can_claim(&issue(Visibility::Public, None), "carol"));
        assert!(!can_claim(&issue(Visibility::Private, None), "carol"));
        assert!(can_claim(&issue(Visibility::Private, None), "alice"));
        let mut done = issue(Visibility::Public, None);
        done.status = Status::Done;
        assert!(!can_claim(&done, "carol"), "finished work is not claimable");
    }

    #[test]
    fn admins_take_over_issues_of_deactivated_issuers() {
        let mut i = issue(Visibility::Public, None);
        i.issuer_active = false;
        assert!(can_manage(&i, &user("root", true)));
        assert!(
            can_change_status(&i, &user("root", true)),
            "takeover includes status"
        );
        assert!(!can_manage(&i, &user("carol", false)));
    }

    #[test]
    fn release_keeps_visibility_for_public_or_own_issues() {
        assert!(can_view_after_release(
            &issue(Visibility::Public, Some("bob")),
            "bob"
        ));
        assert!(!can_view_after_release(
            &issue(Visibility::Private, Some("bob")),
            "bob"
        ));
        assert!(can_view_after_release(
            &issue(Visibility::Private, Some("alice")),
            "alice"
        ));
    }
}
