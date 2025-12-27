/**
 * Wallet Configuration for BETTER Terminal
 * Supports: MetaMask (BASE chain), Phantom (Solana)
 * 
 * BASE Chain Details:
 * - Chain ID: 8453
 * - RPC: https://mainnet.base.org
 * - Explorer: https://basescan.org
 * - Native: ETH
 * 
 * BETTER Token will be deployed on BASE chain
 * 
 * To enable full wagmi support, install:
 * npm install wagmi viem @tanstack/react-query
 */

// BASE Chain configuration (manual, no wagmi dependency)
export const BASE_CHAIN = {
  id: 8453,
  name: 'Base',
  network: 'base',
  nativeCurrency: {
    decimals: 18,
    name: 'Ethereum',
    symbol: 'ETH',
  },
  rpcUrls: {
    default: { http: ['https://mainnet.base.org'] },
    public: { http: ['https://mainnet.base.org'] },
  },
  blockExplorers: {
    default: { name: 'BaseScan', url: 'https://basescan.org' },
  },
};

export const BASE_SEPOLIA = {
  id: 84532,
  name: 'Base Sepolia',
  network: 'base-sepolia',
  nativeCurrency: {
    decimals: 18,
    name: 'Ethereum',
    symbol: 'ETH',
  },
  rpcUrls: {
    default: { http: ['https://sepolia.base.org'] },
    public: { http: ['https://sepolia.base.org'] },
  },
  blockExplorers: {
    default: { name: 'BaseScan', url: 'https://sepolia.basescan.org' },
  },
};

// BETTER Token contract addresses (to be deployed)
export const BETTER_TOKEN = {
  base: '0x0000000000000000000000000000000000000000', // TBD after deployment
  baseSepolia: '0x0000000000000000000000000000000000000000', // TBD for testnet
};

// Vault contract addresses (to be deployed)
export const BETTER_VAULT = {
  base: '0x0000000000000000000000000000000000000000', // TBD after deployment
  baseSepolia: '0x0000000000000000000000000000000000000000', // TBD for testnet
};

// Chain ID for main network
export const DEFAULT_CHAIN_ID = BASE_CHAIN.id;
