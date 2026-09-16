# Reviews

Outside reviewers — other models, in their own sessions — have found more
defects in this code than its tests have: a shutdown hole, a race, a
reachable panic, an ordering bug, and two instances of doc rot. The ways of
asking that produced those:

- **Name the invariant and ask them to break it.** "Try to construct an
  interleaving where a child is spawned and never stopped" beats "review
  `host.rs`".
- **Ask for a falsifier, not an opinion.** The first ownership defect was
  proved by a scratch crate, not argued in prose. A review that cannot fail
  is a review that will agree with you.
- **Ask for questions rather than answers** when what you want attacked is
  the *frame* — whether the plan asks the right thing at all.
- **Have them measure claims in-repo.** When a claim about a dependency or a
  platform is in question, reading its source tells you what it does; only
  measuring tells you how it affects *our* shutdown.
- **Tell them not to consolidate context** if you expect more than one
  exchange.
- **Have them write the work product to disk**, not only into the
  conversation, so it survives and can be argued with later.

Two asymmetries to plan around: a reviewer may have no tools to message you,
in which case it answers in its own turn and you read the transcript; and a
mutating call generally reaches only sessions the caller created, while an
ambient notification reaches any.

When a review lands, the code and the prose both move. Each time the two
drifted here, a reviewer caught it — so when you change a mechanism, grep the
prose for its old description in the same change.
