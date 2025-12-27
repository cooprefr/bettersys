/**
 * Unified Wallet Hook for BETTER Terminal
 * Handles both MetaMask (BASE) and Phantom (Solana) connections
 */

import { useState, useCallback, useEffect } from 'react';

// Wallet types
export type WalletType = 'metamask' | 'phantom' | null;

export interface WalletState {
  isConnected: boolean;
  address: string | null;
  walletType: WalletType;
  chainId: number | null;
  balance: string | null;
}

interface UseWalletReturn extends WalletState {
  connect: (type: WalletType) => Promise<void>;
  disconnect: () => void;
  isLoading: boolean;
  error: string | null;
}

// Check if MetaMask is available
const isMetaMaskAvailable = (): boolean => {
  return typeof window !== 'undefined' && 
         typeof window.ethereum !== 'undefined' && 
         window.ethereum.isMetaMask === true;
};

// Check if Phantom is available
const isPhantomAvailable = (): boolean => {
  return typeof window !== 'undefined' && 
         typeof (window as any).phantom?.solana !== 'undefined';
};

// BASE chain configuration
const BASE_CHAIN = {
  chainId: '0x2105', // 8453 in hex
  chainName: 'Base',
  nativeCurrency: {
    name: 'Ethereum',
    symbol: 'ETH',
    decimals: 18,
  },
  rpcUrls: ['https://mainnet.base.org'],
  blockExplorerUrls: ['https://basescan.org'],
};

export function useWallet(): UseWalletReturn {
  const [state, setState] = useState<WalletState>({
    isConnected: false,
    address: null,
    walletType: null,
    chainId: null,
    balance: null,
  });
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Connect to MetaMask
  const connectMetaMask = useCallback(async () => {
    if (!isMetaMaskAvailable()) {
      throw new Error('MetaMask not installed. Please install MetaMask extension.');
    }

    const ethereum = window.ethereum!;

    // Request account access
    const accounts = await ethereum.request({ 
      method: 'eth_requestAccounts' 
    }) as string[];

    if (!accounts || accounts.length === 0) {
      throw new Error('No accounts found');
    }

    // Switch to BASE chain
    try {
      await ethereum.request({
        method: 'wallet_switchEthereumChain',
        params: [{ chainId: BASE_CHAIN.chainId }],
      });
    } catch (switchError: any) {
      // Chain not added, add it
      if (switchError.code === 4902) {
        await ethereum.request({
          method: 'wallet_addEthereumChain',
          params: [BASE_CHAIN],
        });
      } else {
        throw switchError;
      }
    }

    // Get chain ID
    const chainId = await ethereum.request({ method: 'eth_chainId' }) as string;

    // Get balance
    const balance = await ethereum.request({
      method: 'eth_getBalance',
      params: [accounts[0], 'latest'],
    }) as string;

    setState({
      isConnected: true,
      address: accounts[0],
      walletType: 'metamask',
      chainId: parseInt(chainId, 16),
      balance: (parseInt(balance, 16) / 1e18).toFixed(4),
    });

    // Store in localStorage
    localStorage.setItem('betterbot_wallet_type', 'metamask');
    localStorage.setItem('betterbot_wallet_address', accounts[0]);
  }, []);

  // Connect to Phantom (Solana)
  const connectPhantom = useCallback(async () => {
    if (!isPhantomAvailable()) {
      throw new Error('Phantom not installed. Please install Phantom wallet.');
    }

    const phantom = (window as any).phantom.solana;

    // Request connection
    const response = await phantom.connect();
    const publicKey = response.publicKey.toString();

    setState({
      isConnected: true,
      address: publicKey,
      walletType: 'phantom',
      chainId: null, // Solana doesn't use chain IDs like EVM
      balance: null, // Would need Solana RPC to get balance
    });

    // Store in localStorage
    localStorage.setItem('betterbot_wallet_type', 'phantom');
    localStorage.setItem('betterbot_wallet_address', publicKey);
  }, []);

  // Main connect function
  const connect = useCallback(async (type: WalletType) => {
    setIsLoading(true);
    setError(null);

    try {
      if (type === 'metamask') {
        await connectMetaMask();
      } else if (type === 'phantom') {
        await connectPhantom();
      }
    } catch (err: any) {
      setError(err.message || 'Failed to connect wallet');
      console.error('Wallet connection error:', err);
    } finally {
      setIsLoading(false);
    }
  }, [connectMetaMask, connectPhantom]);

  // Disconnect
  const disconnect = useCallback(() => {
    setState({
      isConnected: false,
      address: null,
      walletType: null,
      chainId: null,
      balance: null,
    });
    localStorage.removeItem('betterbot_wallet_type');
    localStorage.removeItem('betterbot_wallet_address');
  }, []);

  // Auto-reconnect on mount
  useEffect(() => {
    const savedType = localStorage.getItem('betterbot_wallet_type') as WalletType;
    if (savedType) {
      connect(savedType).catch(() => {
        // Silent fail on auto-reconnect
        localStorage.removeItem('betterbot_wallet_type');
        localStorage.removeItem('betterbot_wallet_address');
      });
    }
  }, [connect]);

  // Listen for account/chain changes (MetaMask)
  useEffect(() => {
    if (!isMetaMaskAvailable()) return;

    const ethereum = window.ethereum!;

    const handleAccountsChanged = (accounts: string[]) => {
      if (accounts.length === 0) {
        disconnect();
      } else if (state.walletType === 'metamask') {
        setState(prev => ({ ...prev, address: accounts[0] }));
      }
    };

    const handleChainChanged = (chainId: string) => {
      if (state.walletType === 'metamask') {
        setState(prev => ({ ...prev, chainId: parseInt(chainId, 16) }));
      }
    };

    ethereum.on('accountsChanged', handleAccountsChanged);
    ethereum.on('chainChanged', handleChainChanged);

    return () => {
      ethereum.removeListener('accountsChanged', handleAccountsChanged);
      ethereum.removeListener('chainChanged', handleChainChanged);
    };
  }, [state.walletType, disconnect]);

  return {
    ...state,
    connect,
    disconnect,
    isLoading,
    error,
  };
}

// TypeScript declarations for window.ethereum
declare global {
  interface Window {
    ethereum?: {
      isMetaMask?: boolean;
      request: (args: { method: string; params?: any[] }) => Promise<any>;
      on: (event: string, callback: (...args: any[]) => void) => void;
      removeListener: (event: string, callback: (...args: any[]) => void) => void;
    };
  }
}
