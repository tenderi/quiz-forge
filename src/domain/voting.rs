//! Voting on a question (DESIGN.md §5.3): accepted the moment the deciding ✅
//! lands, rejected when the window runs out first.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vote {
    Yes,
    No,
}

impl Vote {
    pub fn as_str(self) -> &'static str {
        match self {
            Vote::Yes => "yes",
            Vote::No => "no",
        }
    }

    pub fn parse(s: &str) -> Option<Vote> {
        match s {
            "yes" | "y" => Some(Vote::Yes),
            "no" | "n" => Some(Vote::No),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Accepted,
    Rejected,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Accepted => "accepted",
            Outcome::Rejected => "rejected",
        }
    }

    pub fn parse(s: &str) -> Option<Outcome> {
        match s {
            "accepted" => Some(Outcome::Accepted),
            "rejected" => Some(Outcome::Rejected),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoteConfig {
    pub window: Duration,
    pub min_approvals: u32,
}

impl Default for VoteConfig {
    fn default() -> Self {
        VoteConfig {
            window: Duration::days(7),
            min_approvals: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Round {
    pub author_id: i64,
    pub opened_at: DateTime<Utc>,
    pub closes_at: DateTime<Utc>,
    pub votes: BTreeMap<i64, Vote>,
    pub outcome: Option<Outcome>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum VoteError {
    #[error("you can't vote on your own question")]
    AuthorCannotVote,
    #[error("voting on this question is already closed")]
    Closed,
    #[error("the voting window has run out")]
    Expired,
}

impl Round {
    pub fn open(author_id: i64, now: DateTime<Utc>, cfg: &VoteConfig) -> Round {
        Round {
            author_id,
            opened_at: now,
            closes_at: now + cfg.window,
            votes: BTreeMap::new(),
            outcome: None,
        }
    }

    pub fn tally(&self) -> (u32, u32) {
        let yes = self.votes.values().filter(|v| **v == Vote::Yes).count() as u32;
        (yes, self.votes.len() as u32 - yes)
    }

    /// Records a vote (replacing the voter's earlier one) and returns the outcome
    /// if this vote decided the question.
    pub fn cast(
        &mut self,
        voter: i64,
        vote: Vote,
        now: DateTime<Utc>,
        cfg: &VoteConfig,
    ) -> Result<Option<Outcome>, VoteError> {
        if self.outcome.is_some() {
            return Err(VoteError::Closed);
        }
        if now >= self.closes_at {
            return Err(VoteError::Expired);
        }
        if voter == self.author_id {
            return Err(VoteError::AuthorCannotVote);
        }
        self.votes.insert(voter, vote);
        let (yes, no) = self.tally();
        if accepts(yes, no, cfg) {
            self.outcome = Some(Outcome::Accepted);
            return Ok(self.outcome);
        }
        Ok(None)
    }

    /// Rejects the question if the window has run out undecided.
    pub fn expire(&mut self, now: DateTime<Utc>) -> Option<Outcome> {
        if self.outcome.is_none() && now >= self.closes_at {
            self.outcome = Some(Outcome::Rejected);
            return self.outcome;
        }
        None
    }
}

pub fn accepts(yes: u32, no: u32, cfg: &VoteConfig) -> bool {
    yes >= cfg.min_approvals.max(1) && yes > no
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 1, 12, 0, 0).unwrap()
    }

    #[test]
    fn one_yes_accepts_immediately() {
        let cfg = VoteConfig::default();
        let mut r = Round::open(1, t0(), &cfg);
        assert_eq!(r.closes_at, t0() + Duration::days(7));
        assert_eq!(
            r.cast(2, Vote::Yes, t0(), &cfg),
            Ok(Some(Outcome::Accepted))
        );
        assert_eq!(r.cast(3, Vote::No, t0(), &cfg), Err(VoteError::Closed));
    }

    #[test]
    fn a_no_delays_acceptance() {
        let cfg = VoteConfig::default();
        let mut r = Round::open(1, t0(), &cfg);
        assert_eq!(r.cast(2, Vote::No, t0(), &cfg), Ok(None));
        assert_eq!(r.cast(3, Vote::Yes, t0(), &cfg), Ok(None));
        assert_eq!(r.tally(), (1, 1));
        assert_eq!(
            r.cast(4, Vote::Yes, t0(), &cfg),
            Ok(Some(Outcome::Accepted))
        );
    }

    #[test]
    fn changing_a_vote_counts_once() {
        let cfg = VoteConfig::default();
        let mut r = Round::open(1, t0(), &cfg);
        r.cast(2, Vote::No, t0(), &cfg).unwrap();
        r.cast(3, Vote::No, t0(), &cfg).unwrap();
        assert_eq!(r.cast(4, Vote::Yes, t0(), &cfg), Ok(None));
        assert_eq!(
            r.cast(4, Vote::Yes, t0(), &cfg),
            Ok(None),
            "repeat vote changes nothing"
        );
        assert_eq!(r.tally(), (1, 2));
        // 2 switches sides: 2 yes, 1 no decides it
        assert_eq!(
            r.cast(2, Vote::Yes, t0(), &cfg),
            Ok(Some(Outcome::Accepted))
        );
        assert_eq!(r.tally(), (2, 1));
    }

    #[test]
    fn author_cannot_vote() {
        let cfg = VoteConfig::default();
        let mut r = Round::open(1, t0(), &cfg);
        assert_eq!(
            r.cast(1, Vote::Yes, t0(), &cfg),
            Err(VoteError::AuthorCannotVote)
        );
    }

    #[test]
    fn expiry_rejects_and_blocks_late_votes() {
        let cfg = VoteConfig::default();
        let mut r = Round::open(1, t0(), &cfg);
        assert_eq!(r.expire(t0() + Duration::days(6)), None);
        let end = t0() + Duration::days(7);
        assert_eq!(r.cast(2, Vote::Yes, end, &cfg), Err(VoteError::Expired));
        assert_eq!(r.expire(end), Some(Outcome::Rejected));
        assert_eq!(r.expire(end), None, "idempotent");
    }

    #[test]
    fn min_approvals_is_respected() {
        let cfg = VoteConfig {
            window: Duration::hours(1),
            min_approvals: 2,
        };
        let mut r = Round::open(1, t0(), &cfg);
        assert_eq!(r.cast(2, Vote::Yes, t0(), &cfg), Ok(None));
        assert_eq!(
            r.cast(3, Vote::Yes, t0(), &cfg),
            Ok(Some(Outcome::Accepted))
        );
        // zero is treated as one: someone besides the author must approve
        assert!(!accepts(
            0,
            0,
            &VoteConfig {
                window: Duration::hours(1),
                min_approvals: 0
            }
        ));
    }

    #[test]
    fn string_forms() {
        assert_eq!(Vote::parse(Vote::Yes.as_str()), Some(Vote::Yes));
        assert_eq!(Vote::parse("n"), Some(Vote::No));
        assert_eq!(Vote::parse("maybe"), None);
        for o in [Outcome::Accepted, Outcome::Rejected] {
            assert_eq!(Outcome::parse(o.as_str()), Some(o));
        }
        assert_eq!(Outcome::parse("x"), None);
        assert!(!VoteError::Closed.to_string().is_empty());
    }
}
