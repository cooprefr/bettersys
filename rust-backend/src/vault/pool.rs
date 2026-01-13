use anyhow::{anyhow, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::vault::{VaultActivityRecord, VaultDb, VaultNavSnapshotRecord, VaultPaperLedger};

#[derive(Debug, Clone, Default)]
pub struct VaultShareState {
    pub total_shares: f64,
    pub user_shares: HashMap<String, f64>,
}

impl VaultShareState {
    pub fn shares_of(&self, wallet: &str) -> f64 {
        self.user_shares
            .get(&wallet.to_lowercase())
            .copied()
            .unwrap_or(0.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultStateResponse {
    pub cash_usdc: f64,
    pub nav_usdc: f64,
    pub total_shares: f64,
    pub nav_per_share: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultDepositRequest {
    pub wallet_address: String,
    pub amount_usdc: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultDepositResponse {
    pub wallet_address: String,
    pub amount_usdc: f64,
    pub shares_minted: f64,
    pub nav_per_share: f64,
    pub total_shares: f64,
    pub nav_usdc: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultWithdrawRequest {
    pub wallet_address: String,
    pub shares: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultWithdrawResponse {
    pub wallet_address: String,
    pub shares_burned: f64,
    pub amount_usdc: f64,
    pub nav_per_share: f64,
    pub total_shares: f64,
    pub nav_usdc: f64,
}

#[derive(Clone)]
pub struct PooledVault {
    pub db: Arc<VaultDb>,
    pub ledger: Arc<Mutex<VaultPaperLedger>>,
    pub shares: Arc<Mutex<VaultShareState>>,
}

impl PooledVault {
    pub fn new(
        db: Arc<VaultDb>,
        ledger: Arc<Mutex<VaultPaperLedger>>,
        shares: Arc<Mutex<VaultShareState>>,
    ) -> Self {
        Self { db, ledger, shares }
    }

    pub async fn state(&self) -> VaultStateResponse {
        let ledger = self.ledger.lock().await;
        let shares = self.shares.lock().await;
        let nav_usdc = approximate_nav_usdc(&ledger);
        let nav_per_share = if shares.total_shares > 0.0 {
            nav_usdc / shares.total_shares
        } else {
            1.0
        };
        VaultStateResponse {
            cash_usdc: ledger.cash_usdc,
            nav_usdc,
            total_shares: shares.total_shares,
            nav_per_share,
        }
    }

    pub async fn deposit(
        &self,
        wallet_address: &str,
        amount_usdc: f64,
    ) -> Result<VaultDepositResponse> {
        let wallet = wallet_address.trim().to_lowercase();
        if wallet.is_empty() {
            return Err(anyhow!("wallet_address required"));
        }
        if !(amount_usdc.is_finite() && amount_usdc > 0.0) {
            return Err(anyhow!("invalid amount"));
        }

        let now = Utc::now().timestamp();
        let mut ledger = self.ledger.lock().await;
        let mut shares = self.shares.lock().await;

        let nav_before = approximate_nav_usdc(&ledger);
        let nav_per_share = if shares.total_shares > 0.0 {
            (nav_before / shares.total_shares).max(1e-9)
        } else {
            1.0
        };
        let shares_minted = amount_usdc / nav_per_share;

        ledger.cash_usdc += amount_usdc;
        shares.total_shares += shares_minted;
        *shares.user_shares.entry(wallet.clone()).or_insert(0.0) += shares_minted;

        let nav_after = approximate_nav_usdc(&ledger);
        let nav_per_share_after = if shares.total_shares > 0.0 {
            nav_after / shares.total_shares
        } else {
            1.0
        };

        self.db
            .upsert_state(ledger.cash_usdc, shares.total_shares, now)
            .await?;
        self.db
            .set_user_shares(&wallet, shares.user_shares[&wallet], now)
            .await?;

        self.db
            .insert_activity(&VaultActivityRecord {
                id: Uuid::new_v4().to_string(),
                ts: now,
                kind: "DEPOSIT".to_string(),
                wallet_address: Some(wallet.clone()),
                amount_usdc: Some(amount_usdc),
                shares: Some(shares_minted),
                token_id: None,
                market_slug: None,
                outcome: None,
                side: None,
                price: None,
                notional_usdc: None,
                strategy: None,
                decision_id: None,
            })
            .await?;
        self.db
            .insert_nav_snapshot(&VaultNavSnapshotRecord {
                id: Uuid::new_v4().to_string(),
                ts: now,
                nav_usdc: nav_after,
                cash_usdc: ledger.cash_usdc,
                positions_value_usdc: (nav_after - ledger.cash_usdc).max(0.0),
                total_shares: shares.total_shares,
                nav_per_share: nav_per_share_after,
                source: "deposit".to_string(),
            })
            .await?;

        Ok(VaultDepositResponse {
            wallet_address: wallet,
            amount_usdc,
            shares_minted,
            nav_per_share: nav_per_share_after,
            total_shares: shares.total_shares,
            nav_usdc: nav_after,
        })
    }

    pub async fn withdraw(
        &self,
        wallet_address: &str,
        shares_to_burn: f64,
    ) -> Result<VaultWithdrawResponse> {
        let wallet = wallet_address.trim().to_lowercase();
        if wallet.is_empty() {
            return Err(anyhow!("wallet_address required"));
        }
        if !(shares_to_burn.is_finite() && shares_to_burn > 0.0) {
            return Err(anyhow!("invalid shares"));
        }

        let now = Utc::now().timestamp();
        let mut ledger = self.ledger.lock().await;
        let mut shares = self.shares.lock().await;

        let user_shares = shares.shares_of(&wallet);
        if user_shares + 1e-9 < shares_to_burn {
            return Err(anyhow!("insufficient shares"));
        }
        if shares.total_shares <= 0.0 {
            return Err(anyhow!("vault has no shares"));
        }

        let nav_before = approximate_nav_usdc(&ledger);
        let nav_per_share = (nav_before / shares.total_shares).max(1e-9);
        let amount_usdc = shares_to_burn * nav_per_share;

        if ledger.cash_usdc + 1e-9 < amount_usdc {
            return Err(anyhow!(
                "insufficient cash (positions liquidation not implemented)"
            ));
        }

        ledger.cash_usdc -= amount_usdc;
        shares.total_shares -= shares_to_burn;
        let new_user = (user_shares - shares_to_burn).max(0.0);
        if new_user <= 0.0 {
            shares.user_shares.remove(&wallet);
        } else {
            shares.user_shares.insert(wallet.clone(), new_user);
        }

        let nav_after = approximate_nav_usdc(&ledger);
        let nav_per_share_after = if shares.total_shares > 0.0 {
            nav_after / shares.total_shares
        } else {
            1.0
        };

        self.db
            .upsert_state(ledger.cash_usdc, shares.total_shares, now)
            .await?;
        self.db
            .set_user_shares(&wallet, shares.shares_of(&wallet), now)
            .await?;

        self.db
            .insert_activity(&VaultActivityRecord {
                id: Uuid::new_v4().to_string(),
                ts: now,
                kind: "WITHDRAW".to_string(),
                wallet_address: Some(wallet.clone()),
                amount_usdc: Some(amount_usdc),
                shares: Some(shares_to_burn),
                token_id: None,
                market_slug: None,
                outcome: None,
                side: None,
                price: None,
                notional_usdc: None,
                strategy: None,
                decision_id: None,
            })
            .await?;
        self.db
            .insert_nav_snapshot(&VaultNavSnapshotRecord {
                id: Uuid::new_v4().to_string(),
                ts: now,
                nav_usdc: nav_after,
                cash_usdc: ledger.cash_usdc,
                positions_value_usdc: (nav_after - ledger.cash_usdc).max(0.0),
                total_shares: shares.total_shares,
                nav_per_share: nav_per_share_after,
                source: "withdraw".to_string(),
            })
            .await?;

        Ok(VaultWithdrawResponse {
            wallet_address: wallet,
            shares_burned: shares_to_burn,
            amount_usdc,
            nav_per_share: nav_per_share_after,
            total_shares: shares.total_shares,
            nav_usdc: nav_after,
        })
    }
}

pub fn approximate_nav_usdc(ledger: &VaultPaperLedger) -> f64 {
    let positions_value: f64 = ledger
        .positions
        .values()
        .map(|p| p.shares * p.avg_price)
        .sum();
    (ledger.cash_usdc + positions_value).max(0.0)
}
