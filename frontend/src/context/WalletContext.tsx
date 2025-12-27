/**
 * Wallet Context Provider
 * Provides wallet state and actions throughout the app
 */

import { createContext, useContext, ReactNode } from 'react';
import { useWallet, WalletType, WalletState } from '../hooks/useWallet';

interface WalletContextType extends WalletState {
  connect: (type: WalletType) => Promise<void>;
  disconnect: () => void;
  isLoading: boolean;
  error: string | null;
  isMetaMaskAvailable: boolean;
  isPhantomAvailable: boolean;
}

const WalletContext = createContext<WalletContextType | undefined>(undefined);

export function WalletProvider({ children }: { children: ReactNode }) {
  const wallet = useWallet();

  const isMetaMaskAvailable = typeof window !== 'undefined' && 
    typeof window.ethereum !== 'undefined' && 
    window.ethereum.isMetaMask === true;

  const isPhantomAvailable = typeof window !== 'undefined' && 
    typeof (window as any).phantom?.solana !== 'undefined';

  return (
    <WalletContext.Provider value={{ 
      ...wallet, 
      isMetaMaskAvailable,
      isPhantomAvailable,
    }}>
      {children}
    </WalletContext.Provider>
  );
}

export function useWalletContext() {
  const context = useContext(WalletContext);
  if (context === undefined) {
    throw new Error('useWalletContext must be used within a WalletProvider');
  }
  return context;
}
