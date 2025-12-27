//! Signal Storage Engine
//! Pilot in Command: Data Persistence
//! Mission: Store and retrieve signals at physics-constrained speed

use crate::models::MarketSignal;
use anyhow::Result;
use std::collections::VecDeque;

pub struct SignalStorage {
    signals: VecDeque<MarketSignal>,
    max_size: usize,
}

impl SignalStorage {
    pub async fn new() -> Result<Self> {
        Ok(Self {
            signals: VecDeque::with_capacity(10000),
            max_size: 10000,
        })
    }

    pub async fn store(&mut self, signal: MarketSignal) -> Result<()> {
        if self.signals.len() >= self.max_size {
            self.signals.pop_front();
        }
        self.signals.push_back(signal);
        Ok(())
    }

    pub fn get_recent(&self, limit: usize) -> Vec<MarketSignal> {
        self.signals.iter().rev().take(limit).cloned().collect()
    }

    pub async fn clear(&mut self) -> Result<()> {
        self.signals.clear();
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.signals.len()
    }
}
