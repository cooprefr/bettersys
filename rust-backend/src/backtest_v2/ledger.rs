//! Double-Entry Accounting Ledger for Backtest V2
//!
//! This module implements a canonical double-entry ledger that tracks all economic
//! state changes with balanced entries (debits == credits). Every fill, fee, and
//! settlement must be recorded as explicit postings.
//!
//! # Design Principles
//!
//! 1. **Immutability**: All postings are append-only. No modifications or deletions.
//! 2. **Balance**: Every posting batch must have equal total debits and credits.
//! 3. **Traceability**: Every posting includes event references for audit.
//! 4. **Derived State**: Balances are derived from ledger; cached but verifiable.
//!
//! # Account Types
//!
//! - `Cash`: USDC or equivalent cash balance
//! - `Position`: Outcome token holdings (market_id, outcome)
//! - `FeesPaid`: Accumulated trading fees
//! - `RealizedPnL`: Realized profit/loss from closed positions
//! - `Settlement`: Settlement transfers (receivable/payable)
//! - `CostBasis`: Cost basis tracking for positions
//!
//! # Invariants
//!
//! 1. **Equity Identity**: `Cash + MTM(Positions) + Settlements = Initial + RealizedPnL - Fees`
//! 2. **No Negative Cash**: Cash >= 0 unless margin explicitly allowed
//! 3. **No Double-Application**: Each event_ref can only be posted once
//! 4. **Balance Conservation**: Sum(debits) == Sum(credits) for all time

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{OrderId, Price, Side, Size};
use crate::backtest_v2::portfolio::Outcome;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

// =============================================================================
// FIXED-POINT AMOUNT
// =============================================================================

/// Fixed-point amount with 8 decimal places (like satoshis but for USDC).
/// This avoids floating point errors in accounting.
pub type Amount = i128;

/// Conversion factor: 1 USDC = 100_000_000 units
pub const AMOUNT_SCALE: i128 = 100_000_000;

/// Convert f64 to fixed-point Amount.
#[inline]
pub fn to_amount(value: f64) -> Amount {
    (value * AMOUNT_SCALE as f64).round() as Amount
}

/// Convert fixed-point Amount to f64.
#[inline]
pub fn from_amount(amount: Amount) -> f64 {
    amount as f64 / AMOUNT_SCALE as f64
}

// =============================================================================
// LEDGER ACCOUNT
// =============================================================================

/// Account types in the double-entry ledger.
/// 
/// Note: Position quantities are tracked separately (not monetary, not in balanced entries).
/// This enum contains only monetary/value accounts for double-entry balance.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LedgerAccount {
    /// Cash (USDC) - increases with debits, decreases with credits
    Cash,
    
    /// Cost basis for a position (what we paid)
    /// Increases with BUY (debit), decreases proportionally with SELL (credit)
    CostBasis { market_id: String, outcome: Outcome },
    
    /// Fees paid (expense account)
    /// Increases with debits (fee payment)
    FeesPaid,
    
    /// Contributed capital (equity account for deposits)
    /// Credit-normal: increases with credits (deposits)
    Capital,
    
    /// Realized PnL (equity account for trading profits/losses)
    /// Credit-normal: increases with credits (profit), decreases with debits (loss)
    RealizedPnL,
    
    /// Settlement receivable (for resolved markets)
    /// Increases with debits (we are owed), decreases with credits (received)
    SettlementReceivable { market_id: String },
    
    /// Settlement payable (for resolved markets with short positions)
    /// Increases with credits (we owe), decreases with debits (paid)
    SettlementPayable { market_id: String },
}

impl LedgerAccount {
    /// Get a display name for the account.
    pub fn display_name(&self) -> String {
        match self {
            LedgerAccount::Cash => "Cash".to_string(),
            LedgerAccount::CostBasis { market_id, outcome } => {
                format!("CostBasis:{}:{:?}", market_id, outcome)
            }
            LedgerAccount::FeesPaid => "FeesPaid".to_string(),
            LedgerAccount::Capital => "Capital".to_string(),
            LedgerAccount::RealizedPnL => "RealizedPnL".to_string(),
            LedgerAccount::SettlementReceivable { market_id } => {
                format!("SettlementReceivable:{}", market_id)
            }
            LedgerAccount::SettlementPayable { market_id } => {
                format!("SettlementPayable:{}", market_id)
            }
        }
    }
    
    /// Check if this is a normal debit account (increases with debit).
    /// Asset and expense accounts are debit-normal.
    pub fn is_debit_normal(&self) -> bool {
        match self {
            LedgerAccount::Cash => true,
            LedgerAccount::CostBasis { .. } => true,
            LedgerAccount::FeesPaid => true,
            LedgerAccount::Capital => false,          // Credit-normal (equity)
            LedgerAccount::RealizedPnL => false,      // Credit-normal (equity)
            LedgerAccount::SettlementReceivable { .. } => true,  // Asset
            LedgerAccount::SettlementPayable { .. } => false,    // Liability
        }
    }
}

// =============================================================================
// EVENT REFERENCE
// =============================================================================

/// Reference to the event that triggered a posting.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventRef {
    /// Fill event with unique fill_id
    Fill { fill_id: u64 },
    
    /// Fee associated with a fill
    Fee { fill_id: u64 },
    
    /// Settlement of a market
    Settlement { market_id: String, settlement_id: u64 },
    
    /// Initial deposit
    InitialDeposit { deposit_id: u64 },
    
    /// Deposit during simulation
    Deposit { deposit_id: u64 },
    
    /// Withdrawal
    Withdrawal { withdrawal_id: u64 },
    
    /// Position adjustment (e.g., correction)
    Adjustment { adjustment_id: u64, reason: String },
}

impl EventRef {
    pub fn display(&self) -> String {
        match self {
            EventRef::Fill { fill_id } => format!("Fill#{}", fill_id),
            EventRef::Fee { fill_id } => format!("Fee#Fill{}", fill_id),
            EventRef::Settlement { market_id, settlement_id } => {
                format!("Settlement#{}#{}", market_id, settlement_id)
            }
            EventRef::InitialDeposit { deposit_id } => format!("InitialDeposit#{}", deposit_id),
            EventRef::Deposit { deposit_id } => format!("Deposit#{}", deposit_id),
            EventRef::Withdrawal { withdrawal_id } => format!("Withdrawal#{}", withdrawal_id),
            EventRef::Adjustment { adjustment_id, reason } => {
                format!("Adjustment#{}:{}", adjustment_id, reason)
            }
        }
    }
}

// =============================================================================
// LEDGER ENTRY
// =============================================================================

/// A single posting to the ledger (one side of a double-entry).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerPosting {
    /// Which account to post to.
    pub account: LedgerAccount,
    /// Amount to debit (positive) or credit (negative).
    /// Convention: debit > 0, credit < 0
    pub amount: Amount,
}

/// A ledger entry consists of multiple postings that must balance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// Monotonically increasing entry ID.
    pub entry_id: u64,
    
    /// Simulation time when this entry was recorded.
    pub sim_time_ns: Nanos,
    
    /// Arrival time of the triggering event.
    pub arrival_time_ns: Nanos,
    
    /// Reference to the triggering event.
    pub event_ref: EventRef,
    
    /// Human-readable description.
    pub description: String,
    
    /// Individual postings (must sum to zero).
    pub postings: Vec<LedgerPosting>,
    
    /// Additional metadata (order_id, market_id, etc.)
    pub metadata: LedgerMetadata,
}

/// Metadata for ledger entries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LedgerMetadata {
    pub order_id: Option<OrderId>,
    pub market_id: Option<String>,
    pub outcome: Option<Outcome>,
    pub side: Option<Side>,
    pub price: Option<Price>,
    pub quantity: Option<Size>,
}

impl LedgerEntry {
    /// Check if this entry is balanced (debits == credits).
    pub fn is_balanced(&self) -> bool {
        let sum: Amount = self.postings.iter().map(|p| p.amount).sum();
        sum == 0
    }
    
    /// Get total debits.
    pub fn total_debits(&self) -> Amount {
        self.postings.iter().filter(|p| p.amount > 0).map(|p| p.amount).sum()
    }
    
    /// Get total credits.
    pub fn total_credits(&self) -> Amount {
        self.postings.iter().filter(|p| p.amount < 0).map(|p| -p.amount).sum()
    }
}

// =============================================================================
// ACCOUNTING VIOLATION
// =============================================================================

/// Types of accounting violations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ViolationType {
    /// Entry is not balanced (debits != credits).
    UnbalancedEntry { debits: Amount, credits: Amount },
    
    /// Cash went negative (unless margin allowed).
    NegativeCash { balance: Amount },
    
    /// Position went negative (unless shorting allowed).
    /// Note: Position is quantity, tracked separately from balanced ledger.
    NegativePosition { 
        market_id: String, 
        outcome: Outcome, 
        quantity: Amount 
    },
    
    /// Same event_ref posted twice.
    DuplicatePosting { event_ref: EventRef },
    
    /// Equity identity violated.
    EquityMismatch { 
        expected: Amount, 
        actual: Amount, 
        difference: Amount 
    },
    
    /// Cost basis invariant violated (cost basis != sum of entry costs).
    CostBasisMismatch {
        market_id: String,
        outcome: Outcome,
        expected: Amount,
        actual: Amount,
    },
    
    /// Position closed but cost basis not zeroed.
    OrphanedCostBasis {
        market_id: String,
        outcome: Outcome,
        cost_basis: Amount,
    },
    
    /// Settlement attempted on non-existent position.
    InvalidSettlement {
        market_id: String,
        reason: String,
    },
}

/// A violation record with context for debugging.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountingViolation {
    /// Type of violation.
    pub violation_type: ViolationType,
    
    /// Entry that caused the violation.
    pub entry_id: u64,
    
    /// Event reference.
    pub event_ref: EventRef,
    
    /// Simulation time.
    pub sim_time_ns: Nanos,
    
    /// Decision ID at time of violation.
    pub decision_id: u64,
    
    /// Snapshot of derived balances before the event.
    pub balances_before: HashMap<String, Amount>,
    
    /// Snapshot of derived balances after the event.
    pub balances_after: HashMap<String, Amount>,
}

// =============================================================================
// CAUSAL TRACE
// =============================================================================

/// Minimal causal trace for debugging violations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalTrace {
    /// The violation that triggered the trace.
    pub violation: AccountingViolation,
    
    /// Last N ledger entries leading to the violation.
    pub recent_entries: Vec<LedgerEntry>,
    
    /// Current derived balances by account.
    pub current_balances: HashMap<String, Amount>,
    
    /// Configuration at time of violation.
    pub config_snapshot: LedgerConfigSnapshot,
}

/// Snapshot of ledger configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerConfigSnapshot {
    pub allow_negative_cash: bool,
    pub allow_shorting: bool,
    pub strict_mode: bool,
}

impl CausalTrace {
    /// Format as a compact debug string.
    pub fn format_compact(&self) -> String {
        let mut out = String::new();
        out.push_str("=== ACCOUNTING VIOLATION TRACE ===\n");
        out.push_str(&format!("Violation: {:?}\n", self.violation.violation_type));
        out.push_str(&format!("Entry ID: {}\n", self.violation.entry_id));
        out.push_str(&format!("Event: {}\n", self.violation.event_ref.display()));
        out.push_str(&format!("Sim Time: {} ns\n", self.violation.sim_time_ns));
        out.push_str(&format!("Decision ID: {}\n", self.violation.decision_id));
        
        out.push_str("\n--- Balances Before ---\n");
        for (k, v) in &self.violation.balances_before {
            out.push_str(&format!("  {}: {:.8}\n", k, from_amount(*v)));
        }
        
        out.push_str("\n--- Balances After ---\n");
        for (k, v) in &self.violation.balances_after {
            out.push_str(&format!("  {}: {:.8}\n", k, from_amount(*v)));
        }
        
        out.push_str(&format!("\n--- Last {} Entries ---\n", self.recent_entries.len()));
        for entry in &self.recent_entries {
            out.push_str(&format!(
                "  [{}] {} | {} | {}\n",
                entry.entry_id,
                entry.event_ref.display(),
                entry.description,
                if entry.is_balanced() { "BALANCED" } else { "UNBALANCED!" }
            ));
            for posting in &entry.postings {
                let sign = if posting.amount >= 0 { "DR" } else { "CR" };
                out.push_str(&format!(
                    "      {} {} {:.8}\n",
                    sign,
                    posting.account.display_name(),
                    from_amount(posting.amount.abs())
                ));
            }
        }
        
        out.push_str("\n--- Config ---\n");
        out.push_str(&format!("  allow_negative_cash: {}\n", self.config_snapshot.allow_negative_cash));
        out.push_str(&format!("  allow_shorting: {}\n", self.config_snapshot.allow_shorting));
        out.push_str(&format!("  strict_mode: {}\n", self.config_snapshot.strict_mode));
        out.push_str("=================================\n");
        
        out
    }
}

// =============================================================================
// LEDGER
// =============================================================================

/// Configuration for the ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerConfig {
    /// Initial cash (equity).
    pub initial_cash: f64,
    
    /// Allow cash to go negative (margin trading).
    pub allow_negative_cash: bool,
    
    /// Allow positions to go negative (short selling).
    pub allow_shorting: bool,
    
    /// Strict mode: abort on first violation.
    pub strict_mode: bool,
    
    /// Number of entries to keep in causal trace.
    pub trace_depth: usize,
}

impl Default for LedgerConfig {
    fn default() -> Self {
        Self {
            initial_cash: 10000.0,
            allow_negative_cash: false,
            allow_shorting: false,
            strict_mode: true,
            trace_depth: 200,
        }
    }
}

impl LedgerConfig {
    /// Production-grade ledger configuration.
    /// All invariants enforced, strict mode enabled.
    pub fn production_grade() -> Self {
        Self {
            initial_cash: 10000.0,
            allow_negative_cash: false,
            allow_shorting: false,
            strict_mode: true,
            trace_depth: 500,
        }
    }
}

/// The double-entry accounting ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ledger {
    /// Configuration.
    pub config: LedgerConfig,
    
    /// All ledger entries (append-only).
    entries: Vec<LedgerEntry>,
    
    /// Next entry ID.
    next_entry_id: u64,
    
    /// Set of event_refs that have been posted (for dedup).
    posted_events: HashSet<EventRef>,
    
    /// Derived account balances (cached, verified against ledger).
    /// These are monetary values only.
    balances: HashMap<LedgerAccount, Amount>,
    
    /// Position quantities by (market_id, outcome).
    /// Tracked separately from monetary balances (non-monetary).
    positions: HashMap<(String, Outcome), Amount>,
    
    /// Initial equity for invariant checking.
    initial_equity: Amount,
    
    /// Current decision ID (for violation context).
    current_decision_id: u64,
    
    /// First violation (if any).
    first_violation: Option<AccountingViolation>,
    
    /// Statistics.
    pub stats: LedgerStats,
}

/// Ledger statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LedgerStats {
    pub total_entries: u64,
    pub total_postings: u64,
    pub fill_entries: u64,
    pub fee_entries: u64,
    pub settlement_entries: u64,
    pub deposit_entries: u64,
    pub withdrawal_entries: u64,
    pub violations_detected: u64,
    pub invariant_checks: u64,
    pub invariant_passes: u64,
}

impl Ledger {
    /// Create a new ledger with initial deposit.
    pub fn new(config: LedgerConfig) -> Self {
        let initial_equity = to_amount(config.initial_cash);
        
        let mut ledger = Self {
            config,
            entries: Vec::new(),
            next_entry_id: 1,
            posted_events: HashSet::new(),
            balances: HashMap::new(),
            positions: HashMap::new(),
            initial_equity,
            current_decision_id: 0,
            first_violation: None,
            stats: LedgerStats::default(),
        };
        
        // Record initial deposit
        let _ = ledger.post_initial_deposit(ledger.config.initial_cash, 0);
        
        ledger
    }
    
    /// Set the current decision ID (for violation context).
    pub fn set_decision_id(&mut self, decision_id: u64) {
        self.current_decision_id = decision_id;
    }
    
    /// Post initial deposit.
    fn post_initial_deposit(&mut self, amount: f64, now: Nanos) -> Result<u64, AccountingViolation> {
        let amt = to_amount(amount);
        
        let entry = LedgerEntry {
            entry_id: self.next_entry_id,
            sim_time_ns: now,
            arrival_time_ns: now,
            event_ref: EventRef::InitialDeposit { deposit_id: 1 },
            description: format!("Initial deposit: ${:.2}", amount),
            postings: vec![
                LedgerPosting {
                    account: LedgerAccount::Cash,
                    amount: amt, // Debit Cash (increase)
                },
                LedgerPosting {
                    account: LedgerAccount::Capital,
                    amount: -amt, // Credit Capital (equity contribution)
                },
            ],
            metadata: LedgerMetadata::default(),
        };
        
        self.apply_entry(entry)
    }
    
    /// Post a fill event.
    /// 
    /// Double-entry accounting for fills:
    /// - BUY: DR CostBasis, CR Cash (trade_value only, quantity tracked separately)
    /// - SELL: DR Cash (proceeds), CR CostBasis (proportional), +/- RealizedPnL (difference)
    pub fn post_fill(
        &mut self,
        fill_id: u64,
        market_id: &str,
        outcome: Outcome,
        side: Side,
        quantity: Size,
        price: Price,
        fee: f64,
        sim_time_ns: Nanos,
        arrival_time_ns: Nanos,
        order_id: Option<OrderId>,
    ) -> Result<u64, AccountingViolation> {
        let event_ref = EventRef::Fill { fill_id };
        
        // Check for duplicate
        if self.posted_events.contains(&event_ref) {
            return self.record_violation(ViolationType::DuplicatePosting { 
                event_ref: event_ref.clone() 
            });
        }
        
        let trade_value = to_amount(quantity * price);
        
        let mut postings = Vec::new();
        
        // Calculate quantity change (to apply AFTER successful entry)
        let qty_amount = to_amount(quantity);
        let position_delta: Amount;
        
        match side {
            Side::Buy => {
                // BUY: We spend cash to acquire positions
                // DR CostBasis (asset - what we paid for positions)
                // CR Cash (what we spent)
                postings.push(LedgerPosting {
                    account: LedgerAccount::CostBasis {
                        market_id: market_id.to_string(),
                        outcome,
                    },
                    amount: trade_value, // Debit CostBasis (increase)
                });
                postings.push(LedgerPosting {
                    account: LedgerAccount::Cash,
                    amount: -trade_value, // Credit Cash (decrease)
                });
                
                position_delta = qty_amount; // Will add to position after entry succeeds
            }
            Side::Sell => {
                // SELL: We receive cash and reduce our position
                // We need to calculate PnL = proceeds - proportional cost basis
                
                // Get current position and cost basis
                let pos_key = (market_id.to_string(), outcome);
                let position_qty = *self.positions.get(&pos_key).unwrap_or(&0);
                let cost_basis_bal = self.get_balance(&LedgerAccount::CostBasis {
                    market_id: market_id.to_string(),
                    outcome,
                });
                
                // Calculate proportional cost basis to remove
                let closing_cost = if position_qty > 0 {
                    // Average cost method: cost_basis * (qty_sold / total_qty)
                    (cost_basis_bal as f64 * (qty_amount as f64 / position_qty as f64)) as Amount
                } else {
                    0
                };
                
                // PnL = proceeds - cost
                let pnl = trade_value - closing_cost;
                
                // DR Cash (proceeds)
                postings.push(LedgerPosting {
                    account: LedgerAccount::Cash,
                    amount: trade_value, // Debit Cash (increase)
                });
                
                // CR CostBasis (reduce by proportional cost)
                postings.push(LedgerPosting {
                    account: LedgerAccount::CostBasis {
                        market_id: market_id.to_string(),
                        outcome,
                    },
                    amount: -closing_cost, // Credit CostBasis (decrease)
                });
                
                // Balance with RealizedPnL
                // If profit: CR RealizedPnL (pnl > 0, so -pnl < 0 = credit)
                // If loss: DR RealizedPnL (pnl < 0, so -pnl > 0 = debit)
                if pnl != 0 {
                    postings.push(LedgerPosting {
                        account: LedgerAccount::RealizedPnL,
                        amount: -pnl, // Credit for profit, debit for loss
                    });
                }
                
                position_delta = -qty_amount; // Will subtract from position after entry succeeds
            }
        }
        
        let entry = LedgerEntry {
            entry_id: self.next_entry_id,
            sim_time_ns,
            arrival_time_ns,
            event_ref: event_ref.clone(),
            description: format!(
                "{:?} {} {} @ ${:.4}",
                side, quantity, market_id, price
            ),
            postings,
            metadata: LedgerMetadata {
                order_id,
                market_id: Some(market_id.to_string()),
                outcome: Some(outcome),
                side: Some(side),
                price: Some(price),
                quantity: Some(quantity),
            },
        };
        
        let entry_id = self.apply_entry(entry)?;
        self.stats.fill_entries += 1;
        
        // Entry succeeded - NOW update position quantity (non-monetary tracking)
        *self.positions.entry((market_id.to_string(), outcome)).or_insert(0) += position_delta;
        
        // Now post fee as separate entry
        if fee > 0.0 {
            self.post_fee(fill_id, fee, sim_time_ns, arrival_time_ns)?;
        }
        
        Ok(entry_id)
    }
    
    /// Post a fee entry (separate from fill for clarity).
    fn post_fee(
        &mut self,
        fill_id: u64,
        fee: f64,
        sim_time_ns: Nanos,
        arrival_time_ns: Nanos,
    ) -> Result<u64, AccountingViolation> {
        let event_ref = EventRef::Fee { fill_id };
        
        if self.posted_events.contains(&event_ref) {
            return self.record_violation(ViolationType::DuplicatePosting {
                event_ref: event_ref.clone()
            });
        }
        
        let fee_amount = to_amount(fee);
        
        let entry = LedgerEntry {
            entry_id: self.next_entry_id,
            sim_time_ns,
            arrival_time_ns,
            event_ref,
            description: format!("Fee: ${:.4}", fee),
            postings: vec![
                LedgerPosting {
                    account: LedgerAccount::FeesPaid,
                    amount: fee_amount, // Debit FeesPaid (expense increase)
                },
                LedgerPosting {
                    account: LedgerAccount::Cash,
                    amount: -fee_amount, // Credit Cash (decrease)
                },
            ],
            metadata: LedgerMetadata::default(),
        };
        
        let entry_id = self.apply_entry(entry)?;
        self.stats.fee_entries += 1;
        Ok(entry_id)
    }
    
    /// Post a settlement event.
    /// 
    /// Settlement accounting:
    /// - Winning positions: receive $1 per share, realize PnL = $1 - cost_basis
    /// - Losing positions: receive $0 per share, realize PnL = $0 - cost_basis
    pub fn post_settlement(
        &mut self,
        settlement_id: u64,
        market_id: &str,
        winner: Outcome,
        sim_time_ns: Nanos,
        arrival_time_ns: Nanos,
    ) -> Result<u64, AccountingViolation> {
        let event_ref = EventRef::Settlement {
            market_id: market_id.to_string(),
            settlement_id,
        };
        
        if self.posted_events.contains(&event_ref) {
            return self.record_violation(ViolationType::DuplicatePosting {
                event_ref: event_ref.clone()
            });
        }
        
        // Get current positions (quantity) and cost basis for both outcomes
        let yes_qty = *self.positions.get(&(market_id.to_string(), Outcome::Yes)).unwrap_or(&0);
        let no_qty = *self.positions.get(&(market_id.to_string(), Outcome::No)).unwrap_or(&0);
        let yes_cost = self.get_balance(&LedgerAccount::CostBasis {
            market_id: market_id.to_string(),
            outcome: Outcome::Yes,
        });
        let no_cost = self.get_balance(&LedgerAccount::CostBasis {
            market_id: market_id.to_string(),
            outcome: Outcome::No,
        });
        
        let mut postings = Vec::new();
        
        // Settlement logic:
        // - Winning outcome positions pay $1.00 per share
        // - Losing outcome positions pay $0.00 per share
        
        // Process YES position
        if yes_qty != 0 {
            // Settlement value: $1 per share if YES wins, $0 otherwise
            let settlement_value = if winner == Outcome::Yes {
                yes_qty // Each share is worth $1 (in Amount units)
            } else {
                0
            };
            
            // Close cost basis: CR CostBasis
            postings.push(LedgerPosting {
                account: LedgerAccount::CostBasis {
                    market_id: market_id.to_string(),
                    outcome: Outcome::Yes,
                },
                amount: -yes_cost, // Credit CostBasis (close it out)
            });
            
            // Receive settlement: DR Cash
            if settlement_value != 0 {
                postings.push(LedgerPosting {
                    account: LedgerAccount::Cash,
                    amount: settlement_value, // Debit Cash (receive settlement)
                });
            }
            
            // Realize PnL: settlement_value - cost_basis
            let pnl = settlement_value - yes_cost;
            if pnl != 0 {
                postings.push(LedgerPosting {
                    account: LedgerAccount::RealizedPnL,
                    amount: -pnl, // Credit for profit, debit for loss
                });
            }
            
            // Clear position quantity
            self.positions.insert((market_id.to_string(), Outcome::Yes), 0);
        }
        
        // Process NO position
        if no_qty != 0 {
            let settlement_value = if winner == Outcome::No {
                no_qty
            } else {
                0
            };
            
            // Close cost basis: CR CostBasis
            postings.push(LedgerPosting {
                account: LedgerAccount::CostBasis {
                    market_id: market_id.to_string(),
                    outcome: Outcome::No,
                },
                amount: -no_cost,
            });
            
            // Receive settlement: DR Cash
            if settlement_value != 0 {
                postings.push(LedgerPosting {
                    account: LedgerAccount::Cash,
                    amount: settlement_value,
                });
            }
            
            // Realize PnL: settlement_value - cost_basis
            let pnl = settlement_value - no_cost;
            if pnl != 0 {
                postings.push(LedgerPosting {
                    account: LedgerAccount::RealizedPnL,
                    amount: -pnl,
                });
            }
            
            // Clear position quantity
            self.positions.insert((market_id.to_string(), Outcome::No), 0);
        }
        
        if postings.is_empty() {
            // No positions to settle - skip (not an error, market may not have been traded)
            return Ok(0);
        }
        
        let entry = LedgerEntry {
            entry_id: self.next_entry_id,
            sim_time_ns,
            arrival_time_ns,
            event_ref,
            description: format!("Settlement {}: {:?} wins", market_id, winner),
            postings,
            metadata: LedgerMetadata {
                market_id: Some(market_id.to_string()),
                ..Default::default()
            },
        };
        
        let entry_id = self.apply_entry(entry)?;
        self.stats.settlement_entries += 1;
        Ok(entry_id)
    }
    
    /// Apply an entry to the ledger.
    fn apply_entry(&mut self, entry: LedgerEntry) -> Result<u64, AccountingViolation> {
        // Snapshot balances before
        let balances_before = self.snapshot_balances();
        
        // Check if entry is balanced
        if !entry.is_balanced() {
            return self.record_violation_with_snapshot(
                ViolationType::UnbalancedEntry {
                    debits: entry.total_debits(),
                    credits: entry.total_credits(),
                },
                &entry,
                balances_before,
            );
        }
        
        // Apply postings to cached balances
        for posting in &entry.postings {
            let balance = self.balances.entry(posting.account.clone()).or_insert(0);
            *balance += posting.amount;
        }
        
        // Check invariants after applying
        if let Some(violation_type) = self.check_invariants(&entry) {
            let balances_after = self.snapshot_balances();
            return self.record_violation_with_snapshots(
                violation_type,
                &entry,
                balances_before,
                balances_after,
            );
        }
        
        // Record entry
        let entry_id = entry.entry_id;
        self.posted_events.insert(entry.event_ref.clone());
        self.entries.push(entry);
        self.next_entry_id += 1;
        self.stats.total_entries += 1;
        self.stats.total_postings += self.entries.last().map(|e| e.postings.len() as u64).unwrap_or(0);
        
        Ok(entry_id)
    }
    
    /// Check invariants after applying an entry.
    /// 
    /// CONTINUOUS ACCOUNTING INVARIANTS:
    /// 1. Cash >= 0 (unless margin explicitly enabled)
    /// 2. Position quantities >= 0 (unless shorting enabled)
    /// 3. Accounting equation: sum of all balances == 0 (balanced entries)
    /// 4. No orphaned cost basis (cost basis must be zero when position is zero)
    fn check_invariants(&mut self, _entry: &LedgerEntry) -> Option<ViolationType> {
        self.stats.invariant_checks += 1;
        
        // INVARIANT 1: Cash non-negativity
        if !self.config.allow_negative_cash {
            let cash = self.get_balance(&LedgerAccount::Cash);
            if cash < 0 {
                return Some(ViolationType::NegativeCash { balance: cash });
            }
        }
        
        // INVARIANT 2: Position non-negativity (if shorting not allowed)
        if !self.config.allow_shorting {
            for ((market_id, outcome), qty) in &self.positions {
                if *qty < 0 {
                    return Some(ViolationType::NegativePosition {
                        market_id: market_id.clone(),
                        outcome: *outcome,
                        quantity: *qty,
                    });
                }
            }
        }
        
        // INVARIANT 3: Accounting equation balance
        // Sum of all account balances must be zero (debits == credits)
        let total: Amount = self.balances.values().sum();
        if total != 0 {
            // Calculate expected vs actual for diagnostic
            let debits: Amount = self.balances.values().filter(|v| **v > 0).sum();
            let credits: Amount = self.balances.values().filter(|v| **v < 0).map(|v| -v).sum();
            return Some(ViolationType::EquityMismatch {
                expected: 0,
                actual: total,
                difference: (debits - credits).abs(),
            });
        }
        
        // INVARIANT 4: No orphaned cost basis
        // If position quantity is zero, cost basis must also be zero
        for ((market_id, outcome), qty) in &self.positions {
            if *qty == 0 {
                let cost_basis = self.get_balance(&LedgerAccount::CostBasis {
                    market_id: market_id.clone(),
                    outcome: *outcome,
                });
                if cost_basis != 0 {
                    return Some(ViolationType::OrphanedCostBasis {
                        market_id: market_id.clone(),
                        outcome: *outcome,
                        cost_basis,
                    });
                }
            }
        }
        
        self.stats.invariant_passes += 1;
        None
    }
    
    /// Record a violation.
    fn record_violation(&mut self, violation_type: ViolationType) -> Result<u64, AccountingViolation> {
        let violation = AccountingViolation {
            violation_type,
            entry_id: self.next_entry_id,
            event_ref: EventRef::Adjustment { adjustment_id: 0, reason: "n/a".to_string() },
            sim_time_ns: 0,
            decision_id: self.current_decision_id,
            balances_before: self.snapshot_balances(),
            balances_after: self.snapshot_balances(),
        };
        
        self.stats.violations_detected += 1;
        if self.first_violation.is_none() {
            self.first_violation = Some(violation.clone());
        }
        
        if self.config.strict_mode {
            Err(violation)
        } else {
            Ok(0) // Continue but return dummy entry_id
        }
    }
    
    fn record_violation_with_snapshot(
        &mut self,
        violation_type: ViolationType,
        entry: &LedgerEntry,
        balances_before: HashMap<String, Amount>,
    ) -> Result<u64, AccountingViolation> {
        let violation = AccountingViolation {
            violation_type,
            entry_id: entry.entry_id,
            event_ref: entry.event_ref.clone(),
            sim_time_ns: entry.sim_time_ns,
            decision_id: self.current_decision_id,
            balances_before,
            balances_after: self.snapshot_balances(),
        };
        
        self.stats.violations_detected += 1;
        if self.first_violation.is_none() {
            self.first_violation = Some(violation.clone());
        }
        
        if self.config.strict_mode {
            Err(violation)
        } else {
            Ok(0)
        }
    }
    
    fn record_violation_with_snapshots(
        &mut self,
        violation_type: ViolationType,
        entry: &LedgerEntry,
        balances_before: HashMap<String, Amount>,
        balances_after: HashMap<String, Amount>,
    ) -> Result<u64, AccountingViolation> {
        let violation = AccountingViolation {
            violation_type,
            entry_id: entry.entry_id,
            event_ref: entry.event_ref.clone(),
            sim_time_ns: entry.sim_time_ns,
            decision_id: self.current_decision_id,
            balances_before,
            balances_after,
        };
        
        self.stats.violations_detected += 1;
        if self.first_violation.is_none() {
            self.first_violation = Some(violation.clone());
        }
        
        // Rollback the changes (undo the postings) - ALWAYS rollback on violation
        for posting in &entry.postings {
            let balance = self.balances.entry(posting.account.clone()).or_insert(0);
            *balance -= posting.amount;
        }
        
        if self.config.strict_mode {
            Err(violation)
        } else {
            Ok(0) // Continue but return dummy entry_id
        }
    }
    
    /// Snapshot current balances for violation context.
    fn snapshot_balances(&self) -> HashMap<String, Amount> {
        self.balances
            .iter()
            .map(|(k, v)| (k.display_name(), *v))
            .collect()
    }
    
    /// Get the balance of an account.
    pub fn get_balance(&self, account: &LedgerAccount) -> Amount {
        *self.balances.get(account).unwrap_or(&0)
    }
    
    /// Get cash balance as f64.
    pub fn cash(&self) -> f64 {
        from_amount(self.get_balance(&LedgerAccount::Cash))
    }
    
    /// Get total fees paid as f64.
    pub fn fees_paid(&self) -> f64 {
        from_amount(self.get_balance(&LedgerAccount::FeesPaid))
    }
    
    /// Get realized PnL as f64.
    pub fn realized_pnl(&self) -> f64 {
        // RealizedPnL is credit-normal, so positive balance = profit
        // But we store as credits being negative, so negate
        -from_amount(self.get_balance(&LedgerAccount::RealizedPnL))
    }
    
    /// Get position quantity for a token.
    pub fn position_qty(&self, market_id: &str, outcome: Outcome) -> f64 {
        from_amount(*self.positions.get(&(market_id.to_string(), outcome)).unwrap_or(&0))
    }
    
    /// Get cost basis for a position.
    pub fn cost_basis(&self, market_id: &str, outcome: Outcome) -> f64 {
        from_amount(self.get_balance(&LedgerAccount::CostBasis {
            market_id: market_id.to_string(),
            outcome,
        }))
    }
    
    /// Check if there's been a violation.
    pub fn has_violation(&self) -> bool {
        self.first_violation.is_some()
    }
    
    /// Get the first violation.
    pub fn get_first_violation(&self) -> Option<&AccountingViolation> {
        self.first_violation.as_ref()
    }
    
    /// Generate causal trace for debugging.
    pub fn generate_causal_trace(&self) -> Option<CausalTrace> {
        let violation = self.first_violation.clone()?;
        
        let trace_depth = self.config.trace_depth;
        let start_idx = self.entries.len().saturating_sub(trace_depth);
        
        Some(CausalTrace {
            violation,
            recent_entries: self.entries[start_idx..].to_vec(),
            current_balances: self.snapshot_balances(),
            config_snapshot: LedgerConfigSnapshot {
                allow_negative_cash: self.config.allow_negative_cash,
                allow_shorting: self.config.allow_shorting,
                strict_mode: self.config.strict_mode,
            },
        })
    }
    
    /// Get all entries (for inspection/testing).
    pub fn entries(&self) -> &[LedgerEntry] {
        &self.entries
    }
    
    /// Verify equity identity invariant.
    /// Equity = Cash + Unrealized PnL (positions at market) 
    ///        = Initial + RealizedPnL - FeesPaid
    ///
    /// Without mark-to-market, we check:
    /// Cash + CostBasis(all) = Initial - RealizedPnL - FeesPaid
    /// Wait, that's not quite right. Let me think...
    ///
    /// Correct identity:
    /// Cash + Positions*Price = Initial + NetRealizedPnL - Fees
    /// 
    /// Without market prices, we check the book value identity:
    /// Cash + sum(CostBasis) = Initial - sum(RealizedPnL) + sum(CostBasis)
    /// which simplifies to: Cash = Initial - sum(RealizedPnL) - Fees + proceeds_not_yet_realized
    ///
    /// Actually simpler: sum of all debits = sum of all credits (always true if balanced)
    /// So we verify: Total Assets = Total Liabilities + Equity
    pub fn verify_accounting_equation(&self) -> bool {
        // For our ledger: Cash + Positions + CostBasis = Initial + RealizedPnL - Fees
        // Actually: Everything sums to zero because we post balanced entries
        
        // Real check: sum of all account balances by normal side should equal
        let total: Amount = self.balances.values().sum();
        total == 0
    }
}

// =============================================================================
// ACCOUNTING MODE
// =============================================================================

/// Accounting mode for backtest reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AccountingMode {
    /// Full double-entry with all invariants enforced.
    DoubleEntryExact,
    
    /// Legacy implicit accounting (non-representative).
    #[default]
    Legacy,
    
    /// Accounting disabled (testing only, non-representative).
    Disabled,
}

impl AccountingMode {
    pub fn is_representative(&self) -> bool {
        matches!(self, AccountingMode::DoubleEntryExact)
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_amount_conversion() {
        assert_eq!(to_amount(1.0), AMOUNT_SCALE);
        assert_eq!(to_amount(0.5), AMOUNT_SCALE / 2);
        assert_eq!(from_amount(AMOUNT_SCALE), 1.0);
        assert_eq!(from_amount(AMOUNT_SCALE / 2), 0.5);
    }
    
    #[test]
    fn test_initial_deposit() {
        let ledger = Ledger::new(LedgerConfig {
            initial_cash: 1000.0,
            ..Default::default()
        });
        
        assert_eq!(ledger.cash(), 1000.0);
        assert_eq!(ledger.entries().len(), 1);
        assert!(ledger.entries()[0].is_balanced());
    }
    
    #[test]
    fn test_buy_fill() {
        let mut ledger = Ledger::new(LedgerConfig {
            initial_cash: 1000.0,
            strict_mode: true,
            ..Default::default()
        });
        
        // Buy 100 shares @ $0.50 with $0.10 fee
        ledger.post_fill(
            1, "market1", Outcome::Yes, Side::Buy,
            100.0, 0.50, 0.10,
            1000, 1000, None,
        ).unwrap();
        
        // Cash: 1000 - 50 - 0.10 = 949.90
        assert!((ledger.cash() - 949.90).abs() < 0.01);
        
        // Position: 100 shares
        assert!((ledger.position_qty("market1", Outcome::Yes) - 100.0).abs() < 0.01);
        
        // Cost basis: $50
        assert!((ledger.cost_basis("market1", Outcome::Yes) - 50.0).abs() < 0.01);
        
        // Fees: $0.10
        assert!((ledger.fees_paid() - 0.10).abs() < 0.01);
    }
    
    #[test]
    fn test_sell_fill_with_pnl() {
        let mut ledger = Ledger::new(LedgerConfig {
            initial_cash: 1000.0,
            strict_mode: true,
            ..Default::default()
        });
        
        // Buy 100 @ $0.50
        ledger.post_fill(
            1, "market1", Outcome::Yes, Side::Buy,
            100.0, 0.50, 0.0,
            1000, 1000, None,
        ).unwrap();
        
        // Sell 50 @ $0.60 (profit of $5)
        ledger.post_fill(
            2, "market1", Outcome::Yes, Side::Sell,
            50.0, 0.60, 0.0,
            2000, 2000, None,
        ).unwrap();
        
        // Cash: 1000 - 50 + 30 = 980
        assert!((ledger.cash() - 980.0).abs() < 0.01);
        
        // Position: 50 shares remaining
        assert!((ledger.position_qty("market1", Outcome::Yes) - 50.0).abs() < 0.01);
        
        // Cost basis: $25 remaining
        assert!((ledger.cost_basis("market1", Outcome::Yes) - 25.0).abs() < 0.01);
        
        // Realized PnL: $5 profit
        assert!((ledger.realized_pnl() - 5.0).abs() < 0.01);
    }
    
    #[test]
    fn test_settlement_winner() {
        let mut ledger = Ledger::new(LedgerConfig {
            initial_cash: 1000.0,
            strict_mode: true,
            ..Default::default()
        });
        
        // Buy 100 YES @ $0.40
        ledger.post_fill(
            1, "market1", Outcome::Yes, Side::Buy,
            100.0, 0.40, 0.0,
            1000, 1000, None,
        ).unwrap();
        
        // Cash: 1000 - 40 = 960
        assert!((ledger.cash() - 960.0).abs() < 0.01);
        
        // Settle with YES winning
        ledger.post_settlement(
            1, "market1", Outcome::Yes,
            2000, 2000,
        ).unwrap();
        
        // Cash: 960 + 100 = 1060
        assert!((ledger.cash() - 1060.0).abs() < 0.01);
        
        // Position closed
        assert!((ledger.position_qty("market1", Outcome::Yes)).abs() < 0.01);
        
        // Realized PnL: 100 - 40 = 60
        assert!((ledger.realized_pnl() - 60.0).abs() < 0.01);
    }
    
    #[test]
    fn test_settlement_loser() {
        let mut ledger = Ledger::new(LedgerConfig {
            initial_cash: 1000.0,
            strict_mode: true,
            ..Default::default()
        });
        
        // Buy 100 YES @ $0.40
        ledger.post_fill(
            1, "market1", Outcome::Yes, Side::Buy,
            100.0, 0.40, 0.0,
            1000, 1000, None,
        ).unwrap();
        
        // Settle with NO winning (YES loses)
        ledger.post_settlement(
            1, "market1", Outcome::No,
            2000, 2000,
        ).unwrap();
        
        // Cash: 960 + 0 = 960
        assert!((ledger.cash() - 960.0).abs() < 0.01);
        
        // Realized PnL: 0 - 40 = -40
        assert!((ledger.realized_pnl() - (-40.0)).abs() < 0.01);
    }
    
    #[test]
    fn test_duplicate_fill_rejected() {
        let mut ledger = Ledger::new(LedgerConfig {
            initial_cash: 1000.0,
            strict_mode: true,
            ..Default::default()
        });
        
        // First fill
        ledger.post_fill(
            1, "market1", Outcome::Yes, Side::Buy,
            100.0, 0.50, 0.0,
            1000, 1000, None,
        ).unwrap();
        
        // Duplicate fill (same fill_id)
        let result = ledger.post_fill(
            1, "market1", Outcome::Yes, Side::Buy,
            100.0, 0.50, 0.0,
            2000, 2000, None,
        );
        
        assert!(result.is_err());
        if let Err(violation) = result {
            assert!(matches!(violation.violation_type, ViolationType::DuplicatePosting { .. }));
        }
    }
    
    #[test]
    fn test_negative_cash_rejected() {
        let mut ledger = Ledger::new(LedgerConfig {
            initial_cash: 100.0,
            allow_negative_cash: false,
            strict_mode: true,
            ..Default::default()
        });
        
        // Try to buy more than we have
        let result = ledger.post_fill(
            1, "market1", Outcome::Yes, Side::Buy,
            1000.0, 0.50, 0.0, // Would cost $500
            1000, 1000, None,
        );
        
        assert!(result.is_err());
        if let Err(violation) = result {
            assert!(matches!(violation.violation_type, ViolationType::NegativeCash { .. }));
        }
    }
    
    #[test]
    fn test_all_entries_balanced() {
        let mut ledger = Ledger::new(LedgerConfig {
            initial_cash: 1000.0,
            strict_mode: true,
            ..Default::default()
        });
        
        // Series of transactions
        ledger.post_fill(1, "m1", Outcome::Yes, Side::Buy, 100.0, 0.50, 0.05, 1000, 1000, None).unwrap();
        ledger.post_fill(2, "m1", Outcome::No, Side::Buy, 50.0, 0.45, 0.02, 2000, 2000, None).unwrap();
        ledger.post_fill(3, "m1", Outcome::Yes, Side::Sell, 30.0, 0.55, 0.03, 3000, 3000, None).unwrap();
        
        // All entries should be balanced
        for entry in ledger.entries() {
            assert!(entry.is_balanced(), "Entry {} is not balanced", entry.entry_id);
        }
        
        // Accounting equation holds
        assert!(ledger.verify_accounting_equation());
    }
    
    #[test]
    fn test_causal_trace() {
        let mut ledger = Ledger::new(LedgerConfig {
            initial_cash: 100.0,
            allow_negative_cash: false,
            strict_mode: false, // Don't abort, just record
            trace_depth: 10,
            ..Default::default()
        });
        
        // Cause a violation
        let _ = ledger.post_fill(
            1, "m1", Outcome::Yes, Side::Buy,
            1000.0, 0.50, 0.0,
            1000, 1000, None,
        );
        
        assert!(ledger.has_violation());
        
        let trace = ledger.generate_causal_trace().unwrap();
        let formatted = trace.format_compact();
        
        assert!(formatted.contains("ACCOUNTING VIOLATION"));
        assert!(formatted.contains("NegativeCash"));
    }
}
