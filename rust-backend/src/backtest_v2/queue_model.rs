//! Queue Position Model
//!
//! Tracks FIFO queue position at each price level and models
//! realistic fill behavior including cancel-fill races.

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{OrderId, Price, Side, Size};
use crate::backtest_v2::matching::PriceTicks;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, VecDeque};

/// Queue position entry for a tracked order.
#[derive(Debug, Clone)]
pub struct QueuePosition {
    pub order_id: OrderId,
    /// Position in queue (0 = front of queue).
    pub position: usize,
    /// Size ahead of us in queue.
    pub size_ahead: Size,
    /// Our order size.
    pub our_size: Size,
    /// Time we joined the queue.
    pub joined_at: Nanos,
    /// Last update time.
    pub last_update: Nanos,
}

/// A single price level queue.
#[derive(Debug, Clone, Default)]
struct LevelQueue {
    /// Orders in FIFO order.
    orders: VecDeque<QueueEntry>,
    /// Total size at this level.
    total_size: Size,
}

#[derive(Debug, Clone)]
struct QueueEntry {
    order_id: OrderId,
    size: Size,
    is_ours: bool,
    joined_at: Nanos,
}

impl LevelQueue {
    /// Add an order to the back of the queue.
    fn push_back(&mut self, order_id: OrderId, size: Size, is_ours: bool, now: Nanos) {
        self.total_size += size;
        self.orders.push_back(QueueEntry {
            order_id,
            size,
            is_ours,
            joined_at: now,
        });
    }

    /// Remove an order by ID.
    fn remove(&mut self, order_id: OrderId) -> Option<QueueEntry> {
        if let Some(pos) = self.orders.iter().position(|e| e.order_id == order_id) {
            let entry = self.orders.remove(pos)?;
            self.total_size -= entry.size;
            Some(entry)
        } else {
            None
        }
    }

    /// Reduce front order size (for fills).
    fn reduce_front(&mut self, fill_size: Size) -> Option<(OrderId, Size, bool, bool)> {
        if let Some(front) = self.orders.front_mut() {
            let actual = fill_size.min(front.size);
            front.size -= actual;
            self.total_size -= actual;

            let order_id = front.order_id;
            let is_ours = front.is_ours;
            let fully_filled = front.size <= 0.0;

            if fully_filled {
                self.orders.pop_front();
            }

            Some((order_id, actual, is_ours, fully_filled))
        } else {
            None
        }
    }

    /// Get queue position for an order.
    fn get_position(&self, order_id: OrderId) -> Option<QueuePosition> {
        let mut position = 0;
        let mut size_ahead = 0.0;

        for entry in &self.orders {
            if entry.order_id == order_id {
                return Some(QueuePosition {
                    order_id,
                    position,
                    size_ahead,
                    our_size: entry.size,
                    joined_at: entry.joined_at,
                    last_update: entry.joined_at, // Would be updated by caller
                });
            }
            position += 1;
            size_ahead += entry.size;
        }

        None
    }

    /// Check if order is at front.
    fn is_at_front(&self, order_id: OrderId) -> bool {
        self.orders
            .front()
            .map(|e| e.order_id == order_id)
            .unwrap_or(false)
    }

    fn is_empty(&self) -> bool {
        self.orders.is_empty()
    }

    fn len(&self) -> usize {
        self.orders.len()
    }
}

/// Queue position tracker for one side of the book.
#[derive(Debug)]
struct SideQueues {
    levels: BTreeMap<PriceTicks, LevelQueue>,
}

impl SideQueues {
    fn new() -> Self {
        Self {
            levels: BTreeMap::new(),
        }
    }

    fn get_or_create(&mut self, price: PriceTicks) -> &mut LevelQueue {
        self.levels.entry(price).or_insert_with(LevelQueue::default)
    }

    fn get(&self, price: PriceTicks) -> Option<&LevelQueue> {
        self.levels.get(&price)
    }

    fn get_mut(&mut self, price: PriceTicks) -> Option<&mut LevelQueue> {
        self.levels.get_mut(&price)
    }

    fn remove_level(&mut self, price: PriceTicks) {
        self.levels.remove(&price);
    }
}

/// Order state for in-flight tracking.
#[derive(Debug, Clone)]
pub struct InFlightOrder {
    pub order_id: OrderId,
    pub side: Side,
    pub price_ticks: PriceTicks,
    pub size: Size,
    /// Time order was sent.
    pub sent_at: Nanos,
    /// Expected arrival at venue.
    pub arrives_at: Nanos,
    /// Is this a cancel request?
    pub is_cancel: bool,
    /// Original order ID (for cancels).
    pub target_order_id: Option<OrderId>,
}

/// Cancel-fill race result.
#[derive(Debug, Clone)]
pub enum RaceResult {
    /// Cancel arrived in time, order was cancelled.
    CancelWon {
        order_id: OrderId,
        cancelled_size: Size,
    },
    /// Fill happened first, cancel failed.
    FillWon {
        order_id: OrderId,
        filled_size: Size,
    },
    /// Partial: some filled before cancel arrived.
    Partial {
        order_id: OrderId,
        filled_size: Size,
        cancelled_size: Size,
    },
}

/// Queue position model with race condition handling.
pub struct QueuePositionModel {
    /// Bid side queues (price -> queue).
    bids: SideQueues,
    /// Ask side queues (price -> queue).
    asks: SideQueues,
    /// Our orders by order_id.
    our_orders: HashMap<OrderId, OrderLocation>,
    /// In-flight orders/cancels.
    in_flight: Vec<InFlightOrder>,
    /// Pending cancels (order_id -> cancel arrival time).
    pending_cancels: HashMap<OrderId, Nanos>,
    /// Statistics.
    pub stats: QueueStats,
}

#[derive(Debug, Clone)]
struct OrderLocation {
    side: Side,
    price_ticks: PriceTicks,
}

/// Queue model statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueueStats {
    pub orders_queued: u64,
    pub orders_filled_at_front: u64,
    pub orders_filled_in_queue: u64,
    pub orders_cancelled: u64,
    pub cancels_lost_race: u64,
    pub total_queue_time_ns: i64,
    pub fills_from_queue: u64,
    /// Number of L2 book deltas processed (from `price_change` messages).
    pub deltas_processed: u64,
    /// Total volume removed from queue via deltas.
    pub queue_volume_removed: f64,
    /// Total volume added to queue via deltas.
    pub queue_volume_added: f64,
}

impl QueueStats {
    pub fn avg_queue_time_ns(&self) -> f64 {
        if self.fills_from_queue > 0 {
            self.total_queue_time_ns as f64 / self.fills_from_queue as f64
        } else {
            0.0
        }
    }

    pub fn cancel_race_loss_rate(&self) -> f64 {
        let total_cancels = self.orders_cancelled + self.cancels_lost_race;
        if total_cancels > 0 {
            self.cancels_lost_race as f64 / total_cancels as f64
        } else {
            0.0
        }
    }
}

impl QueuePositionModel {
    pub fn new() -> Self {
        Self {
            bids: SideQueues::new(),
            asks: SideQueues::new(),
            our_orders: HashMap::new(),
            in_flight: Vec::new(),
            pending_cancels: HashMap::new(),
            stats: QueueStats::default(),
        }
    }

    /// Add an order to the queue (from external market data).
    pub fn add_order(
        &mut self,
        order_id: OrderId,
        side: Side,
        price_ticks: PriceTicks,
        size: Size,
        is_ours: bool,
        now: Nanos,
    ) {
        let queues = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };

        let level = queues.get_or_create(price_ticks);
        level.push_back(order_id, size, is_ours, now);

        if is_ours {
            self.our_orders
                .insert(order_id, OrderLocation { side, price_ticks });
            self.stats.orders_queued += 1;
        }
    }

    /// Apply a single-level delta from L2BookDelta event (from `price_change` WebSocket message).
    /// 
    /// This method is called when we receive incremental book updates, enabling precise
    /// queue position tracking for MAKER VIABILITY.
    /// 
    /// - `side`: Side::Buy for bids, Side::Sell for asks
    /// - `price`: The price level affected (will be converted to ticks)
    /// - `new_size`: The NEW aggregate size at this level (0 = level removed)
    /// - `timestamp`: When the delta was observed (arrival time)
    /// 
    /// Note: This is an aggregate-size update, not a per-order update. We use it
    /// to track total size at each level for queue_ahead calculations, but we cannot
    /// track individual order positions without order-level data.
    pub fn apply_delta(&mut self, side: Side, price: Price, new_size: Size, timestamp: Nanos) {
        // Convert price to ticks (assuming cent ticks like Polymarket)
        let price_ticks = (price * 100.0).round() as PriceTicks;
        
        let queues = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };
        
        if new_size <= 0.0 {
            // Level removed
            queues.remove_level(price_ticks);
            self.stats.deltas_processed += 1;
        } else {
            // Update level size
            let level = queues.get_or_create(price_ticks);
            let old_size = level.total_size;
            
            // Adjust the total size by updating a synthetic entry
            // We can't track individual orders, but we can track aggregate size changes
            let size_delta = new_size - old_size;
            level.total_size = new_size;
            
            // If our orders are at this level, update queue_ahead
            // Size delta < 0 means orders ahead of us may have been filled/cancelled
            // Size delta > 0 means new orders joined behind us (no queue position change)
            if size_delta < 0.0 {
                // Volume was removed - this could affect our queue position
                self.stats.queue_volume_removed += (-size_delta);
            } else {
                self.stats.queue_volume_added += size_delta;
            }
            
            self.stats.deltas_processed += 1;
        }
    }

    /// Remove an order from the queue.
    pub fn remove_order(&mut self, order_id: OrderId) -> Option<Size> {
        // Check if it's our order
        let location = self.our_orders.remove(&order_id);

        // Try both sides if we don't know the location
        let (side, price_ticks) = if let Some(loc) = location {
            (loc.side, loc.price_ticks)
        } else {
            // Search both sides (for external orders)
            return self.remove_from_any_side(order_id);
        };

        let queues = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };

        if let Some(level) = queues.get_mut(price_ticks) {
            if let Some(entry) = level.remove(order_id) {
                if level.is_empty() {
                    queues.remove_level(price_ticks);
                }
                return Some(entry.size);
            }
        }

        None
    }

    fn remove_from_any_side(&mut self, order_id: OrderId) -> Option<Size> {
        // Try bids
        for (&price, level) in self.bids.levels.iter_mut() {
            if let Some(entry) = level.remove(order_id) {
                if level.is_empty() {
                    // Mark for removal
                }
                return Some(entry.size);
            }
        }

        // Try asks
        for (&price, level) in self.asks.levels.iter_mut() {
            if let Some(entry) = level.remove(order_id) {
                if level.is_empty() {
                    // Mark for removal
                }
                return Some(entry.size);
            }
        }

        None
    }

    /// Submit our order to the queue (tracks when it will arrive).
    pub fn submit_order(
        &mut self,
        order_id: OrderId,
        side: Side,
        price_ticks: PriceTicks,
        size: Size,
        sent_at: Nanos,
        latency_ns: Nanos,
    ) {
        let arrives_at = sent_at + latency_ns;

        self.in_flight.push(InFlightOrder {
            order_id,
            side,
            price_ticks,
            size,
            sent_at,
            arrives_at,
            is_cancel: false,
            target_order_id: None,
        });
    }

    /// Submit a cancel request.
    pub fn submit_cancel(
        &mut self,
        cancel_id: OrderId,
        target_order_id: OrderId,
        sent_at: Nanos,
        latency_ns: Nanos,
    ) {
        let arrives_at = sent_at + latency_ns;

        // Record pending cancel
        self.pending_cancels.insert(target_order_id, arrives_at);

        // Get target order location if known
        let location = self.our_orders.get(&target_order_id).cloned();

        self.in_flight.push(InFlightOrder {
            order_id: cancel_id,
            side: location.as_ref().map(|l| l.side).unwrap_or(Side::Buy),
            price_ticks: location.as_ref().map(|l| l.price_ticks).unwrap_or(0),
            size: 0.0,
            sent_at,
            arrives_at,
            is_cancel: true,
            target_order_id: Some(target_order_id),
        });
    }

    /// Process in-flight orders that have arrived.
    pub fn process_arrivals(&mut self, now: Nanos) -> Vec<InFlightOrder> {
        let mut arrived = Vec::new();

        self.in_flight.retain(|order| {
            if order.arrives_at <= now {
                arrived.push(order.clone());
                false
            } else {
                true
            }
        });

        // Process non-cancel arrivals
        for order in &arrived {
            if !order.is_cancel {
                self.add_order(
                    order.order_id,
                    order.side,
                    order.price_ticks,
                    order.size,
                    true,
                    order.arrives_at,
                );
            }
        }

        arrived
    }

    /// Process fills at a price level (from market data).
    /// Returns fills that affected our orders.
    pub fn process_fills(
        &mut self,
        side: Side,
        price_ticks: PriceTicks,
        fill_size: Size,
        now: Nanos,
    ) -> Vec<OurFill> {
        // Phase 1: Collect fills from queue
        let mut fill_results: Vec<(OrderId, Size, bool, bool)> = Vec::new();
        let mut remaining = fill_size;
        let mut level_empty = false;

        {
            let queues = match side {
                Side::Buy => &mut self.bids,
                Side::Sell => &mut self.asks,
            };

            let Some(level) = queues.get_mut(price_ticks) else {
                return Vec::new();
            };

            while remaining > 0.0 {
                let Some((order_id, filled, is_ours, fully_filled)) = level.reduce_front(remaining)
                else {
                    break;
                };
                remaining -= filled;
                fill_results.push((order_id, filled, is_ours, fully_filled));
            }

            level_empty = level.is_empty();
        }

        // Phase 2: Remove empty level
        if level_empty {
            match side {
                Side::Buy => self.bids.remove_level(price_ticks),
                Side::Sell => self.asks.remove_level(price_ticks),
            }
        }

        // Phase 3: Process our fills and check races
        let mut fills = Vec::new();

        for (order_id, filled, is_ours, fully_filled) in fill_results {
            if !is_ours {
                continue;
            }

            // Check for cancel race (now safe to call)
            let cancel_arrives = self.pending_cancels.get(&order_id).copied();
            let race_result = if let Some(cancel_time) = cancel_arrives {
                if cancel_time <= now {
                    Some(RaceResult::CancelWon {
                        order_id,
                        cancelled_size: 0.0,
                    })
                } else {
                    Some(RaceResult::FillWon {
                        order_id,
                        filled_size: filled,
                    })
                }
            } else {
                None
            };

            match race_result {
                Some(RaceResult::CancelWon { .. }) => {
                    self.stats.orders_cancelled += 1;
                }
                Some(RaceResult::FillWon { .. }) => {
                    self.stats.cancels_lost_race += 1;
                    fills.push(OurFill {
                        order_id,
                        size: filled,
                        time: now,
                        was_at_front: true,
                    });
                    self.stats.orders_filled_at_front += 1;
                    self.stats.fills_from_queue += 1;
                }
                Some(RaceResult::Partial { filled_size, .. }) => {
                    fills.push(OurFill {
                        order_id,
                        size: filled_size,
                        time: now,
                        was_at_front: true,
                    });
                    self.stats.fills_from_queue += 1;
                }
                None => {
                    fills.push(OurFill {
                        order_id,
                        size: filled,
                        time: now,
                        was_at_front: true,
                    });
                    self.stats.orders_filled_at_front += 1;
                    self.stats.fills_from_queue += 1;
                }
            }

            // Only remove from tracking if the order is fully consumed.
            if fully_filled {
                self.our_orders.remove(&order_id);
                self.pending_cancels.remove(&order_id);
            }
        }

        fills
    }

    /// Check for cancel-fill race condition.
    fn check_cancel_race(&self, order_id: OrderId, fill_time: Nanos) -> Option<RaceResult> {
        let cancel_arrives = self.pending_cancels.get(&order_id)?;

        if *cancel_arrives <= fill_time {
            // Cancel arrived before fill
            Some(RaceResult::CancelWon {
                order_id,
                cancelled_size: 0.0, // Would need to track original size
            })
        } else {
            // Fill happened first
            Some(RaceResult::FillWon {
                order_id,
                filled_size: 0.0, // Caller knows the size
            })
        }
    }

    /// Get queue position for one of our orders.
    pub fn get_position(&self, order_id: OrderId) -> Option<QueuePosition> {
        let location = self.our_orders.get(&order_id)?;

        let queues = match location.side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        let level = queues.get(location.price_ticks)?;
        level.get_position(order_id)
    }

    /// Get all our queue positions.
    pub fn get_all_positions(&self) -> Vec<QueuePosition> {
        self.our_orders
            .keys()
            .filter_map(|&id| self.get_position(id))
            .collect()
    }

    /// Estimate probability of fill given current queue position.
    pub fn estimate_fill_probability(
        &self,
        order_id: OrderId,
        expected_volume: Size,
    ) -> Option<f64> {
        let pos = self.get_position(order_id)?;

        if pos.size_ahead <= 0.0 {
            // We're at the front
            Some(1.0)
        } else if expected_volume <= 0.0 {
            Some(0.0)
        } else {
            // Simple model: prob = min(1, expected_volume / (size_ahead + our_size))
            let total_to_fill = pos.size_ahead + pos.our_size;
            Some((expected_volume / total_to_fill).min(1.0))
        }
    }

    /// Reset state.
    pub fn reset(&mut self) {
        self.bids = SideQueues::new();
        self.asks = SideQueues::new();
        self.our_orders.clear();
        self.in_flight.clear();
        self.pending_cancels.clear();
        self.stats = QueueStats::default();
    }
}

impl Default for QueuePositionModel {
    fn default() -> Self {
        Self::new()
    }
}

/// Fill event for our order.
#[derive(Debug, Clone)]
pub struct OurFill {
    pub order_id: OrderId,
    pub size: Size,
    pub time: Nanos,
    pub was_at_front: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_queue_position_tracking() {
        let mut model = QueuePositionModel::new();

        // Add orders to queue
        model.add_order(1, Side::Buy, 45, 100.0, false, 1000); // External
        model.add_order(2, Side::Buy, 45, 50.0, true, 2000); // Ours
        model.add_order(3, Side::Buy, 45, 75.0, false, 3000); // External

        // Our order should be at position 1 with 100 size ahead
        let pos = model.get_position(2).unwrap();
        assert_eq!(pos.position, 1);
        assert_eq!(pos.size_ahead, 100.0);
        assert_eq!(pos.our_size, 50.0);
    }

    #[test]
    fn test_fill_from_front() {
        let mut model = QueuePositionModel::new();

        model.add_order(1, Side::Buy, 45, 100.0, true, 1000); // Ours at front
        model.add_order(2, Side::Buy, 45, 50.0, false, 2000); // External behind

        // Process fill
        let fills = model.process_fills(Side::Buy, 45, 80.0, 3000);

        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].order_id, 1);
        assert_eq!(fills[0].size, 80.0);
        assert!(fills[0].was_at_front);

        // Our order should have 20 remaining
        let pos = model.get_position(1).unwrap();
        assert_eq!(pos.our_size, 20.0);
    }

    #[test]
    fn test_fill_consumes_queue() {
        let mut model = QueuePositionModel::new();

        model.add_order(1, Side::Buy, 45, 50.0, false, 1000); // External at front
        model.add_order(2, Side::Buy, 45, 50.0, true, 2000); // Ours second

        // Initially we have 50 size ahead
        let pos = model.get_position(2).unwrap();
        assert_eq!(pos.size_ahead, 50.0);

        // Fill 50 (consumes external order)
        model.process_fills(Side::Buy, 45, 50.0, 3000);

        // Now we should be at front
        let pos = model.get_position(2).unwrap();
        assert_eq!(pos.position, 0);
        assert_eq!(pos.size_ahead, 0.0);
    }

    #[test]
    fn test_cancel_race_fill_wins() {
        let mut model = QueuePositionModel::new();

        // Add our order
        model.add_order(1, Side::Buy, 45, 100.0, true, 1000);

        // Submit cancel that will arrive at t=5000
        model.submit_cancel(99, 1, 2000, 3000);

        // Fill happens at t=3000 (before cancel arrives)
        let fills = model.process_fills(Side::Buy, 45, 100.0, 3000);

        assert_eq!(fills.len(), 1);
        assert_eq!(model.stats.cancels_lost_race, 1);
    }

    #[test]
    fn test_in_flight_processing() {
        let mut model = QueuePositionModel::new();

        // Submit order that will arrive at t=2000
        model.submit_order(1, Side::Buy, 45, 100.0, 1000, 1000);

        // At t=1500, order hasn't arrived
        let arrived = model.process_arrivals(1500);
        assert!(arrived.is_empty());
        assert!(model.get_position(1).is_none());

        // At t=2000, order arrives
        let arrived = model.process_arrivals(2000);
        assert_eq!(arrived.len(), 1);
        assert!(model.get_position(1).is_some());
    }

    #[test]
    fn test_fill_probability_estimation() {
        let mut model = QueuePositionModel::new();

        model.add_order(1, Side::Buy, 45, 100.0, false, 1000);
        model.add_order(2, Side::Buy, 45, 100.0, true, 2000);

        // With 100 ahead and 100 ours, need 200 volume for 100% fill
        let prob = model.estimate_fill_probability(2, 200.0).unwrap();
        assert!((prob - 1.0).abs() < 0.001);

        // With 50 volume, ~25% chance
        let prob = model.estimate_fill_probability(2, 50.0).unwrap();
        assert!((prob - 0.25).abs() < 0.001);
    }
}
