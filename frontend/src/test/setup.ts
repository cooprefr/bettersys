/**
 * Vitest Test Setup
 * 
 * Sets up the test environment for frontend tests.
 */

import { expect, afterEach } from 'vitest';
import * as matchers from '@testing-library/jest-dom/matchers';

// Extend vitest expect with jest-dom matchers
expect.extend(matchers);

// Reset any global state after each test
afterEach(() => {
  // Clear any mocks
  // Reset timezone config (done in individual test files)
});

// Mock window.matchMedia if needed for responsive tests
Object.defineProperty(window, 'matchMedia', {
  writable: true,
  value: (query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: () => {},
    removeListener: () => {},
    addEventListener: () => {},
    removeEventListener: () => {},
    dispatchEvent: () => false,
  }),
});

// Mock IntersectionObserver if needed
class MockIntersectionObserver {
  observe = () => {};
  disconnect = () => {};
  unobserve = () => {};
}

Object.defineProperty(window, 'IntersectionObserver', {
  writable: true,
  value: MockIntersectionObserver,
});
