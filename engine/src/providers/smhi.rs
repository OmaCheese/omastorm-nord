//! The SMHI provider: `smhi_live.rs`'s poller (S3) and the Sweden composite
//! (S8) behind the provider interface.
//!
//! SMHI publishes one volume per radar, and one composite, every 5 minutes
//! under CC BY 4.0 ("SMHI"). A Swedish station's id is its SMHI area key
//! (DEC-12), so the poller's area and the events' station are the same
//! string, and `problems` refuses an SMHI row whose `sourceId` differs.

use super::{Event, ProviderId, Spec, Staleness};
use crate::protocol::Station;
use crate::smhi_live;
use std::time::Duration;
use tokio::sync::mpsc::Sender;

/// The credit SMHI's licence asks for.
pub const ATTRIBUTION: &str = "SMHI, CC BY 4.0";
/// One volume per radar every 5 minutes.
const CADENCE: Duration = Duration::from_secs(5 * 60);

pub const SPEC: Spec = Spec {
    id: ProviderId::Smhi,
    name: "SMHI",
    attribution: ATTRIBUTION,
    country: "SE",
    // The lowest tilt's 480 gates of 500 m, the first centred at 250 m:
    // the last gate's far edge is 240 km out.
    range_km: 240.0,
    cadence: CADENCE,
    staleness: Staleness::from_cadence(CADENCE),
    backfill: smhi_live::BACKFILL,
    ranges: smhi_live::DEC2,
};

/// Poll SMHI for `station`: its qcvol listing, or the composite's for
/// `sweden`, following `want` (S20).
pub async fn poll(
    station: Station,
    events: Sender<Event>,
    cached: Vec<i64>,
    skip_known: bool,
    want: crate::products::Want,
) {
    smhi_live::poll(station.id, events, cached, skip_known, want).await;
}
