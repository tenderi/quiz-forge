//! Who may change a question and how (DESIGN.md §5.4).

use super::model::{Content, Field, Patch};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Voting,
    Accepted,
    Rejected,
    Retired,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Voting => "voting",
            Status::Accepted => "accepted",
            Status::Rejected => "rejected",
            Status::Retired => "retired",
        }
    }

    pub fn parse(s: &str) -> Option<Status> {
        match s {
            "voting" => Some(Status::Voting),
            "accepted" => Some(Status::Accepted),
            "rejected" => Some(Status::Rejected),
            "retired" => Some(Status::Retired),
            _ => None,
        }
    }
}

/// What happens when the author edits their own question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorEdit {
    /// Applied at once. `reopen_vote`: start a new voting round.
    Apply { reopen_vote: bool },
    /// Becomes a proposal that another member has to approve.
    NeedsSecondApproval,
}

pub fn author_edit(status: Status, patch: &Patch) -> AuthorEdit {
    match status {
        Status::Voting => AuthorEdit::Apply {
            reopen_vote: patch.is_material(),
        },
        // Revising a rejected question resubmits it.
        Status::Rejected => AuthorEdit::Apply { reopen_vote: true },
        Status::Accepted | Status::Retired => {
            if patch.is_material() {
                AuthorEdit::NeedsSecondApproval
            } else {
                AuthorEdit::Apply { reopen_vote: false }
            }
        }
    }
}

/// Whether accepting someone else's proposal starts a new voting round.
pub fn reopens_vote(status: Status, patch: &Patch) -> bool {
    status == Status::Voting && patch.is_material()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProposalView {
    pub proposer_id: i64,
    pub needs_second_approval: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DecideError {
    #[error("only the question's author can decide on this proposal")]
    NotAuthor,
    #[error("you can't approve your own proposal")]
    OwnProposal,
}

/// Checks that `decider` may accept or decline a proposal. Orphaned questions
/// (author gone from the group) can be decided by any member but the proposer.
pub fn may_decide(
    p: ProposalView,
    author_id: i64,
    author_in_group: bool,
    decider: i64,
) -> Result<(), DecideError> {
    if decider == p.proposer_id {
        return Err(DecideError::OwnProposal);
    }
    if p.needs_second_approval || decider == author_id || !author_in_group {
        return Ok(());
    }
    Err(DecideError::NotAuthor)
}

/// A proposal made against an older version, re-checked against the current one.
#[derive(Debug, Clone, PartialEq)]
pub enum Rebase {
    /// Still applies; the patch is unchanged.
    Clean,
    /// Someone changed the same fields since; the proposer has to redo it.
    Stale(Vec<Field>),
    /// The current version already has these values.
    AlreadyApplied,
}

pub fn rebase(base: &Content, current: &Content, patch: &Patch) -> Rebase {
    let moved = Patch::between(base, current);
    let clash: Vec<Field> = patch.fields().filter(|f| moved.0.contains_key(f)).collect();
    if !clash.is_empty() {
        let applied = patch.apply(current).ok();
        if applied.as_ref() == Some(current) {
            return Rebase::AlreadyApplied;
        }
        return Rebase::Stale(clash);
    }
    Rebase::Clean
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Content {
        Content::text("Capital of Finland?", "Helsinki", "geography", "en")
    }

    fn patch_with(f: impl FnOnce(&mut Content)) -> Patch {
        let mut c = base();
        f(&mut c);
        Patch::between(&base(), &c)
    }

    #[test]
    fn author_edit_paths() {
        let material = patch_with(|c| c.answer = "Hki".into());
        let minor = patch_with(|c| c.fun_fact = "x".into());
        assert_eq!(
            author_edit(Status::Voting, &material),
            AuthorEdit::Apply { reopen_vote: true }
        );
        assert_eq!(
            author_edit(Status::Voting, &minor),
            AuthorEdit::Apply { reopen_vote: false }
        );
        assert_eq!(
            author_edit(Status::Rejected, &minor),
            AuthorEdit::Apply { reopen_vote: true }
        );
        assert_eq!(
            author_edit(Status::Accepted, &material),
            AuthorEdit::NeedsSecondApproval
        );
        assert_eq!(
            author_edit(Status::Retired, &material),
            AuthorEdit::NeedsSecondApproval
        );
        assert_eq!(
            author_edit(Status::Accepted, &minor),
            AuthorEdit::Apply { reopen_vote: false }
        );
        assert!(reopens_vote(Status::Voting, &material));
        assert!(!reopens_vote(Status::Accepted, &material));
        assert!(!reopens_vote(Status::Voting, &minor));
    }

    #[test]
    fn deciding_rules() {
        let outside = ProposalView {
            proposer_id: 5,
            needs_second_approval: false,
        };
        assert_eq!(may_decide(outside, 1, true, 1), Ok(()));
        assert_eq!(may_decide(outside, 1, true, 7), Err(DecideError::NotAuthor));
        assert_eq!(
            may_decide(outside, 1, true, 5),
            Err(DecideError::OwnProposal)
        );
        // orphaned question: anyone but the proposer
        assert_eq!(may_decide(outside, 1, false, 7), Ok(()));
        assert_eq!(
            may_decide(outside, 1, false, 5),
            Err(DecideError::OwnProposal)
        );
        // author's own material edit to an accepted question
        let own = ProposalView {
            proposer_id: 1,
            needs_second_approval: true,
        };
        assert_eq!(may_decide(own, 1, true, 1), Err(DecideError::OwnProposal));
        assert_eq!(may_decide(own, 1, true, 9), Ok(()));
        assert!(!DecideError::NotAuthor.to_string().is_empty());
    }

    #[test]
    fn rebase_outcomes() {
        let b = base();
        let mut cur = b.clone();
        cur.tags = vec!["nordic".into()];
        let p = patch_with(|c| c.fun_fact = "Founded 1550.".into());
        assert_eq!(rebase(&b, &cur, &p), Rebase::Clean);
        assert_eq!(rebase(&b, &b, &p), Rebase::Clean);

        let mut cur2 = b.clone();
        cur2.fun_fact = "Something else.".into();
        assert_eq!(rebase(&b, &cur2, &p), Rebase::Stale(vec![Field::FunFact]));

        let mut cur3 = b.clone();
        cur3.fun_fact = "Founded 1550.".into();
        assert_eq!(rebase(&b, &cur3, &p), Rebase::AlreadyApplied);
    }

    #[test]
    fn status_strings() {
        for s in [
            Status::Voting,
            Status::Accepted,
            Status::Rejected,
            Status::Retired,
        ] {
            assert_eq!(Status::parse(s.as_str()), Some(s));
        }
        assert_eq!(Status::parse("draft"), None);
    }
}
