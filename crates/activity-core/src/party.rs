//! Parties, in the way of Dota 2's: a small group (up to five) with one leader, the **DJ**, who decides what plays for
//! everyone. People are invited, accept or decline; only the DJ can pass the lead on or send someone away; when the
//! DJ leaves, the person who has been in the party longest takes over. This module is only the rules, with no network.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const MAX_MEMBERS: usize = 5;
/// An invitation that is not answered is forgotten after this long.
pub const INVITE_TTL_MS: u64 = 2 * 60_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartyView {
    pub id: u64,
    pub leader: String,
    /// In the order they joined (the DJ's successor is the first one after them).
    pub members: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteView {
    pub party_id: u64,
    pub from: String,
    pub leader: String,
    pub members: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PartyError {
    NotInParty,
    NotLeader,
    Full,
    AlreadyMember,
    InvitedElsewhere,
    InviteeBusy,
    NoSuchInvite,
    BusyInOtherParty,
    NotMember,
    Yourself,
}

impl PartyError {
    pub fn message(&self) -> &'static str {
        match self {
            PartyError::NotInParty => "you are not in a party",
            PartyError::NotLeader => "only the DJ can do that",
            PartyError::Full => "the party is full",
            PartyError::AlreadyMember => "that person is already in the party",
            PartyError::InvitedElsewhere => "that invitation is not for you",
            PartyError::InviteeBusy => "that person is already in a party",
            PartyError::NoSuchInvite => "that invitation is gone",
            PartyError::BusyInOtherParty => "leave your party first",
            PartyError::NotMember => "that person is not in the party",
            PartyError::Yourself => "that is you",
        }
    }
}

struct Party {
    id: u64,
    leader: String,
    members: Vec<String>,
}

struct Invite {
    party_id: u64,
    from: String,
    at_ms: u64,
}

#[derive(Default)]
pub struct Parties {
    next_id: u64,
    by_id: HashMap<u64, Party>,
    member_of: HashMap<String, u64>,
    invites: HashMap<String, Vec<Invite>>,
}

impl Parties {
    fn view(p: &Party) -> PartyView {
        PartyView { id: p.id, leader: p.leader.clone(), members: p.members.clone() }
    }

    pub fn party_of(&self, user: &str) -> Option<PartyView> {
        self.member_of.get(user).and_then(|id| self.by_id.get(id)).map(Self::view)
    }

    /// Everyone in the party `user` is in (or `user` alone).
    pub fn mates_of(&self, user: &str) -> Vec<String> {
        self.party_of(user).map_or_else(|| vec![user.to_string()], |p| p.members)
    }

    fn expire(&mut self, now_ms: u64) {
        for list in self.invites.values_mut() {
            list.retain(|i| now_ms.saturating_sub(i.at_ms) < INVITE_TTL_MS);
        }
        self.invites.retain(|_, l| !l.is_empty());
        let alive: Vec<u64> = self.by_id.keys().copied().collect();
        for list in self.invites.values_mut() {
            list.retain(|i| alive.contains(&i.party_id));
        }
    }

    pub fn invites_for(&mut self, user: &str, now_ms: u64) -> Vec<InviteView> {
        self.expire(now_ms);
        self.invites
            .get(user)
            .into_iter()
            .flatten()
            .filter_map(|i| self.by_id.get(&i.party_id).map(|p| InviteView { party_id: p.id, from: i.from.clone(), leader: p.leader.clone(), members: p.members.clone() }))
            .collect()
    }

    /// `from` invites `to`. A person not in a party starts one (and is its DJ). Any member may invite.
    pub fn invite(&mut self, from: &str, to: &str, now_ms: u64) -> Result<u64, PartyError> {
        if from == to {
            return Err(PartyError::Yourself);
        }
        self.expire(now_ms);
        if let Some(theirs) = self.member_of.get(to) {
            // Someone already in the same party is a different error from someone in another party; a person alone
            // in a party of their own (they only invited someone) may still be invited elsewhere.
            if self.member_of.get(from) == Some(theirs) {
                return Err(PartyError::AlreadyMember);
            }
            if self.by_id.get(theirs).is_some_and(|p| p.members.len() > 1) {
                return Err(PartyError::InviteeBusy);
            }
        }
        let id = match self.member_of.get(from) {
            Some(id) => *id,
            None => {
                self.next_id += 1;
                let id = self.next_id;
                self.by_id.insert(id, Party { id, leader: from.to_string(), members: vec![from.to_string()] });
                self.member_of.insert(from.to_string(), id);
                id
            }
        };
        let party = self.by_id.get(&id).expect("the party exists");
        let pending = self.invites.values().flatten().filter(|i| i.party_id == id && now_ms.saturating_sub(i.at_ms) < INVITE_TTL_MS).count();
        let already = self.invites.get(to).is_some_and(|l| l.iter().any(|i| i.party_id == id));
        if !already && party.members.len() + pending >= MAX_MEMBERS {
            return Err(PartyError::Full);
        }
        let list = self.invites.entry(to.to_string()).or_default();
        list.retain(|i| i.party_id != id);
        list.push(Invite { party_id: id, from: from.to_string(), at_ms: now_ms });
        Ok(id)
    }

    /// `user` joins the party they were invited to. A person alone in a party of their own simply leaves it.
    pub fn accept(&mut self, user: &str, party_id: u64, now_ms: u64) -> Result<PartyView, PartyError> {
        self.expire(now_ms);
        if !self.invites.get(user).is_some_and(|l| l.iter().any(|i| i.party_id == party_id)) {
            return Err(PartyError::NoSuchInvite);
        }
        if let Some(mine) = self.party_of(user) {
            if mine.members.len() > 1 {
                return Err(PartyError::BusyInOtherParty);
            }
            self.leave(user);
        }
        let party = self.by_id.get_mut(&party_id).ok_or(PartyError::NoSuchInvite)?;
        if party.members.len() >= MAX_MEMBERS {
            return Err(PartyError::Full);
        }
        party.members.push(user.to_string());
        self.member_of.insert(user.to_string(), party_id);
        self.invites.remove(user);
        Ok(Self::view(self.by_id.get(&party_id).expect("the party exists")))
    }

    pub fn decline(&mut self, user: &str, party_id: u64) -> bool {
        let Some(list) = self.invites.get_mut(user) else { return false };
        let before = list.len();
        list.retain(|i| i.party_id != party_id);
        let changed = list.len() != before;
        if list.is_empty() {
            self.invites.remove(user);
        }
        changed
    }

    /// `user` leaves. When the DJ leaves, the person who has been there longest takes over; an empty party is gone.
    /// Returns the party as it is now, if it still exists, and who was in it before.
    pub fn leave(&mut self, user: &str) -> Option<(Option<PartyView>, Vec<String>)> {
        let id = self.member_of.remove(user)?;
        let party = self.by_id.get_mut(&id)?;
        let before = party.members.clone();
        party.members.retain(|m| m != user);
        if party.members.is_empty() {
            self.by_id.remove(&id);
            return Some((None, before));
        }
        if party.leader == user {
            party.leader = party.members[0].clone();
        }
        Some((Some(Self::view(self.by_id.get(&id).expect("the party exists"))), before))
    }

    /// The DJ hands the lead to another member.
    pub fn set_leader(&mut self, by: &str, to: &str) -> Result<PartyView, PartyError> {
        let id = *self.member_of.get(by).ok_or(PartyError::NotInParty)?;
        let party = self.by_id.get_mut(&id).ok_or(PartyError::NotInParty)?;
        if party.leader != by {
            return Err(PartyError::NotLeader);
        }
        if by == to {
            return Err(PartyError::Yourself);
        }
        if !party.members.iter().any(|m| m == to) {
            return Err(PartyError::NotMember);
        }
        party.leader = to.to_string();
        Ok(Self::view(party))
    }

    /// The DJ sends a member away.
    pub fn kick(&mut self, by: &str, who: &str) -> Result<PartyView, PartyError> {
        let id = *self.member_of.get(by).ok_or(PartyError::NotInParty)?;
        let party = self.by_id.get_mut(&id).ok_or(PartyError::NotInParty)?;
        if party.leader != by {
            return Err(PartyError::NotLeader);
        }
        if by == who {
            return Err(PartyError::Yourself);
        }
        if !party.members.iter().any(|m| m == who) {
            return Err(PartyError::NotMember);
        }
        party.members.retain(|m| m != who);
        self.member_of.remove(who);
        Ok(Self::view(self.by_id.get(&id).expect("the party exists")))
    }

    /// Two people no longer want to share anything (one blocked the other): invitations between them go, and if they
    /// are in one party the blocked person is sent away, or, when that is the DJ, the blocker leaves.
    pub fn separate(&mut self, blocker: &str, blocked: &str) -> Vec<String> {
        for (to, from) in [(blocker, blocked), (blocked, blocker)] {
            if let Some(list) = self.invites.get_mut(to) {
                list.retain(|i| i.from != from);
            }
        }
        let (Some(a), Some(b)) = (self.member_of.get(blocker).copied(), self.member_of.get(blocked).copied()) else { return Vec::new() };
        if a != b {
            return Vec::new();
        }
        let leader = self.by_id.get(&a).map(|p| p.leader.clone()).unwrap_or_default();
        let leaving = if leader == blocked { blocker } else { blocked };
        self.leave(leaving).map(|(_, before)| before).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn party_of_three() -> Parties {
        let mut p = Parties::default();
        let id = p.invite("alice", "bob", 0).unwrap();
        p.accept("bob", id, 1).unwrap();
        p.invite("alice", "cara", 2).unwrap();
        p.accept("cara", id, 3).unwrap();
        p
    }

    #[test]
    fn inviting_starts_a_party_with_the_inviter_as_dj() {
        let mut p = Parties::default();
        let id = p.invite("alice", "bob", 0).unwrap();
        let v = p.party_of("alice").unwrap();
        assert_eq!((v.id, v.leader.as_str(), v.members.len()), (id, "alice", 1));
        assert_eq!(p.invites_for("bob", 1).len(), 1);
        assert!(p.party_of("bob").is_none(), "an invitation is not a membership");
        assert_eq!(p.invite("alice", "alice", 0), Err(PartyError::Yourself));
    }

    #[test]
    fn accepting_joins_in_order_and_declining_forgets() {
        let mut p = party_of_three();
        let v = p.party_of("cara").unwrap();
        assert_eq!((v.leader.as_str(), v.members.clone()), ("alice", vec!["alice".to_string(), "bob".into(), "cara".into()]));
        let id = p.invite("alice", "dan", 5).unwrap();
        assert!(p.decline("dan", id));
        assert_eq!(p.accept("dan", id, 6), Err(PartyError::NoSuchInvite));
        assert!(!p.decline("dan", id));
    }

    #[test]
    fn any_member_may_invite_but_a_party_holds_five_and_nobody_in_another_party() {
        let mut p = party_of_three();
        assert!(p.invite("bob", "dan", 5).is_ok(), "any member may invite");
        assert!(p.invite("cara", "erin", 5).is_ok());
        // Three members and two invitations make five: no more room.
        assert_eq!(p.invite("alice", "fay", 5), Err(PartyError::Full));
        let mut q = Parties::default();
        let id = q.invite("gus", "hal", 0).unwrap();
        q.accept("hal", id, 1).unwrap();
        q.invite("ivy", "jon", 0).unwrap();
        // Someone already in a party of two or more cannot be invited to another.
        assert_eq!(q.invite("ivy", "hal", 2), Err(PartyError::InviteeBusy));
        assert_eq!(q.invite("gus", "hal", 2), Err(PartyError::AlreadyMember));
    }

    #[test]
    fn invitations_run_out() {
        let mut p = Parties::default();
        p.invite("alice", "bob", 0).unwrap();
        assert_eq!(p.invites_for("bob", INVITE_TTL_MS - 1).len(), 1);
        assert!(p.invites_for("bob", INVITE_TTL_MS).is_empty());
    }

    #[test]
    fn someone_alone_in_their_own_party_may_accept_another() {
        let mut p = Parties::default();
        p.invite("alice", "bob", 0).unwrap(); // alice is alone in a party of her own
        let id = p.invite("cara", "alice", 0).unwrap();
        let joined = p.accept("alice", id, 1).unwrap();
        assert_eq!(joined.members, vec!["cara".to_string(), "alice".into()]);
        assert!(p.invites_for("bob", 2).is_empty(), "alice's empty party and its invitation are gone");
    }

    #[test]
    fn only_the_dj_passes_the_lead_or_sends_someone_away() {
        let mut p = party_of_three();
        assert_eq!(p.set_leader("bob", "cara"), Err(PartyError::NotLeader));
        assert_eq!(p.kick("bob", "cara"), Err(PartyError::NotLeader));
        assert_eq!(p.set_leader("alice", "alice"), Err(PartyError::Yourself));
        assert_eq!(p.set_leader("alice", "zed"), Err(PartyError::NotMember));
        assert_eq!(p.set_leader("alice", "bob").unwrap().leader, "bob");
        assert_eq!(p.kick("alice", "cara"), Err(PartyError::NotLeader), "alice is no longer the DJ");
        let after = p.kick("bob", "cara").unwrap();
        assert_eq!(after.members, vec!["alice".to_string(), "bob".into()]);
        assert!(p.party_of("cara").is_none());
    }

    #[test]
    fn when_the_dj_leaves_the_longest_member_takes_over_and_an_empty_party_ends() {
        let mut p = party_of_three();
        let (now, before) = p.leave("alice").unwrap();
        assert_eq!(before.len(), 3);
        assert_eq!(now.unwrap().leader, "bob");
        p.leave("cara");
        let (gone, _) = p.leave("bob").unwrap();
        assert!(gone.is_none());
        assert!(p.party_of("bob").is_none());
        assert!(p.leave("bob").is_none(), "leaving twice does nothing");
    }

    #[test]
    fn blocking_sends_the_blocked_person_away_or_the_blocker_when_that_is_the_dj() {
        let mut p = party_of_three();
        let affected = p.separate("bob", "cara");
        assert_eq!(affected.len(), 3);
        assert!(p.party_of("cara").is_none());
        assert!(p.party_of("bob").is_some());
        let mut q = party_of_three();
        q.separate("bob", "alice"); // the blocked one is the DJ: bob leaves
        assert!(q.party_of("bob").is_none());
        assert_eq!(q.party_of("alice").unwrap().members.len(), 2);
        let mut r = Parties::default();
        r.invite("alice", "bob", 0).unwrap();
        r.separate("bob", "alice");
        assert!(r.invites_for("bob", 1).is_empty(), "an invitation from someone blocked is dropped");
    }
}
