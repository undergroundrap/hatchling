//! The broker's skills, published once at its start: what every run's
//! session reaches (WO-046). A test binary of its own, so the
//! process-wide value set here reaches no other test.

use hatchling_broker::Skills;
use hatchling_broker::skills::{live, publish};

#[test]
fn the_first_skills_published_are_the_ones_every_run_reaches() {
    assert!(live().is_none(), "nothing until the broker publishes");
    let first = publish(Skills::default());
    let reached = live().expect("published");
    assert!(std::ptr::eq(first, reached), "the very skills published");
    // A broker publishes once: a second is not what runs reach.
    let second = publish(Skills::default());
    assert!(std::ptr::eq(second, first));
    assert!(std::ptr::eq(live().expect("published"), first));
}
