#!/usr/bin/env python3
"""Executable model of packaging/macos/domain-registrar/main.swift's
DomainRemovalGrace. Swift cannot be compiled in the default devenv
shell (Nix SDK, no swiftc), so this model pins the state machine that
the boot-race fix depends on. It must change together with the Swift
class; test-dev-install.sh pins the source markers.

Four transitions are covered:
  1. cold boot — no live observation yet, so no removal on any tick;
  2. owners publish — each live id is observed;
  3. a genuine owner death — six misses, removed on the sixth;
  4. one-shot manual run — removes on the first miss.
Also covers the same-tick add/prune regression: a domain registered
in a tick keeps its live observation when prune runs in that tick.
"""

GRACE_TICKS = 6


class Grace:
    def __init__(self):
        self.missing = {}
        self.seen_live = set()

    def observe_present(self, ident):
        self.seen_live.add(ident)
        self.missing[ident] = 0

    def should_remove(self, ident, enabled=True):
        if not enabled:
            return True
        if ident not in self.seen_live:
            self.missing.pop(ident, None)
            return False
        streak = self.missing.get(ident, 0) + 1
        self.missing[ident] = streak
        return streak >= GRACE_TICKS

    def prune(self, ids):
        for ident in list(self.missing):
            if ident not in ids:
                self.missing.pop(ident)
                self.seen_live.discard(ident)
        for ident in list(self.seen_live):
            if ident not in ids:
                self.seen_live.discard(ident)


def tick(grace, existing, desired):
    for ident in desired:
        grace.observe_present(ident)
    removed = {i for i in existing - desired if grace.should_remove(i)}
    # Mirrors reconcile(): current is snapshotted before the add loop,
    # so ids added this tick must be kept explicitly at prune time.
    grace.prune(existing.union(desired))
    return removed


def check(label, condition):
    if not condition:
        raise AssertionError(label)
    print(f"  ok: {label}")


def main():
    # 1. Cold boot: registrar starts before the owners publish. Three
    # ticks with an empty `desired` must remove nothing.
    grace = Grace()
    existing = {"beta", "alpha"}
    for tick_no in range(3):
        check(f"boot tick {tick_no} removes nothing", not tick(grace, existing, set()))
    check("boot records no live observation", not grace.seen_live)

    # 2. Owners publish: both ids are observed live, nothing removed.
    check("publish tick removes nothing", not tick(grace, existing, existing))
    check("publish records both ids live", grace.seen_live == existing)

    # 3. A genuine owner death: deferred for five ticks, removed on six.
    for tick_no in range(1, GRACE_TICKS):
        removed = tick(grace, existing, {"alpha"})
        check(f"death tick {tick_no} defers", "beta" not in removed)
    check(
        f"death tick {GRACE_TICKS} removes the dead domain",
        "beta" in tick(grace, existing, {"alpha"}),
    )

    # 4. One-shot has no history and removes on the first miss.
    one_shot = Grace()
    check("one-shot removes on first miss", one_shot.should_remove("beta", enabled=False))

    # 5. Same-tick add/prune: a domain registered while its id was
    # absent from the pre-add `current` snapshot keeps its observation.
    fresh = Grace()
    known = set()
    added = {"beta"}
    fresh.observe_present("beta")
    fresh.prune(known.union(added))  # reconcile's post-add prune
    check("same-tick add keeps its live observation", "beta" in fresh.seen_live)
    for tick_no in range(1, GRACE_TICKS):
        check(f"new domain death tick {tick_no} defers", not fresh.should_remove("beta"))
    check(
        f"new domain death tick {GRACE_TICKS} removes",
        fresh.should_remove("beta"),
    )

    # 6. Prune: a domain macOS no longer reports (removed outside the
    # loop) loses both its streak and its live observation, so it
    # cannot be resurrected by a later tick.
    pruned = Grace()
    pruned.observe_present("beta")
    pruned.missing["beta"] = 3
    pruned.prune(set())  # macOS reports no domains
    check("prune drops the stale streak", "beta" not in pruned.missing)
    check("prune drops the stale live observation", "beta" not in pruned.seen_live)
    check("pruned domain cannot be removed again", not pruned.should_remove("beta"))

    print("domain grace model: all checks passed")


if __name__ == "__main__":
    main()
