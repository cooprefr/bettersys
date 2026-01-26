import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, fireEvent } from '@testing-library/react';
import { CertifiedHistogram } from '../CertifiedHistogram';
import type { WindowPnlHistogramResponse } from '../../../types/backtest';
import { HISTOGRAM_SCHEMA_VERSION, validateHistogramResponse } from '../../../types/backtest';

// Mock the API
vi.mock('../../../services/api', () => ({
  api: {
    getWindowPnlHistogram: vi.fn(),
  },
}));

import { api } from '../../../services/api';
const mockApi = api as { getWindowPnlHistogram: ReturnType<typeof vi.fn> };

// Fixture: valid histogram response
const createValidHistogram = (binCount: number = 10): WindowPnlHistogramResponse => {
  const bins = Array.from({ length: binCount }, (_, i) => ({
    left: -50 + i * 10,
    right: -50 + (i + 1) * 10,
    count: Math.floor(Math.random() * 20) + 1,
  }));

  const totalSamples = bins.reduce((sum, b) => sum + b.count, 0);

  return {
    schema_version: HISTOGRAM_SCHEMA_VERSION,
    run_id: 'run_test123abc',
    manifest_hash: 'deadbeef12345678',
    unit: 'USD',
    binning: {
      method: 'fixed_edges',
      bin_count: binCount,
      min: -50,
      max: 50,
    },
    bins,
    underflow_count: 0,
    overflow_count: 0,
    total_samples: totalSamples,
    trust_level: 'Trusted',
    is_trusted: true,
  };
};

describe('CertifiedHistogram', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  describe('Rendering with preloaded data', () => {
    it('renders histogram bars from preloaded data', () => {
      const histogram = createValidHistogram(5);
      render(<CertifiedHistogram runId="run_test" data={histogram} />);

      // Check that the SVG is rendered
      const svg = screen.getByTestId('certified-histogram-svg');
      expect(svg).toBeInTheDocument();

      // Check that we have the correct number of bars
      for (let i = 0; i < histogram.bins.length; i++) {
        const bar = screen.getByTestId(`histogram-bar-${i}`);
        expect(bar).toBeInTheDocument();
      }
    });

    it('displays provenance badge with run_id and manifest_hash', () => {
      const histogram = createValidHistogram(5);
      render(<CertifiedHistogram runId="run_test" data={histogram} />);

      // Check for truncated hashes
      expect(screen.getByText('test123a')).toBeInTheDocument(); // run_id truncated
      expect(screen.getByText('deadbeef')).toBeInTheDocument(); // manifest_hash truncated
    });

    it('shows CERTIFIED badge for trusted runs', () => {
      const histogram = createValidHistogram(5);
      histogram.is_trusted = true;
      render(<CertifiedHistogram runId="run_test" data={histogram} />);

      expect(screen.getByText('CERTIFIED')).toBeInTheDocument();
    });

    it('shows UNCERTIFIED badge for untrusted runs', () => {
      const histogram = createValidHistogram(5);
      histogram.is_trusted = false;
      render(<CertifiedHistogram runId="run_test" data={histogram} />);

      expect(screen.getByText('UNCERTIFIED')).toBeInTheDocument();
    });

    it('displays total samples and bin count', () => {
      const histogram = createValidHistogram(10);
      render(<CertifiedHistogram runId="run_test" data={histogram} />);

      expect(screen.getByText(new RegExp(`${histogram.total_samples} WINDOWS`))).toBeInTheDocument();
      expect(screen.getByText(/10 BINS/)).toBeInTheDocument();
      expect(screen.getByText(/USD/)).toBeInTheDocument();
    });
  });

  describe('Bar positioning from bin edges', () => {
    it('renders bars with correct data attributes from edges', () => {
      const histogram: WindowPnlHistogramResponse = {
        schema_version: 'v1',
        run_id: 'run_test',
        manifest_hash: 'hash123',
        unit: 'USD',
        binning: { method: 'fixed_edges', bin_count: 3, min: -10, max: 20 },
        bins: [
          { left: -10, right: 0, count: 5 },
          { left: 0, right: 10, count: 15 },
          { left: 10, right: 20, count: 8 },
        ],
        underflow_count: 0,
        overflow_count: 0,
        total_samples: 28,
        trust_level: 'Trusted',
        is_trusted: true,
      };

      render(<CertifiedHistogram runId="run_test" data={histogram} />);

      // Check that each bar has correct edge data attributes
      histogram.bins.forEach((bin, i) => {
        const bar = screen.getByTestId(`histogram-bar-${i}`);
        expect(bar).toHaveAttribute('data-left', String(bin.left));
        expect(bar).toHaveAttribute('data-right', String(bin.right));
        expect(bar).toHaveAttribute('data-count', String(bin.count));
      });
    });
  });

  describe('Download functionality', () => {
    it('has download button', () => {
      const histogram = createValidHistogram(5);
      render(<CertifiedHistogram runId="run_test" data={histogram} />);

      expect(screen.getByText('[DOWNLOAD JSON]')).toBeInTheDocument();
    });

    it('calls onDownload callback when provided', () => {
      const histogram = createValidHistogram(5);
      const onDownload = vi.fn();
      render(<CertifiedHistogram runId="run_test" data={histogram} onDownload={onDownload} />);

      fireEvent.click(screen.getByText('[DOWNLOAD JSON]'));

      expect(onDownload).toHaveBeenCalledOnce();
      const jsonArg = onDownload.mock.calls[0][0];
      expect(JSON.parse(jsonArg)).toEqual(histogram);
    });
  });

  describe('Error states', () => {
    it('shows error state for invalid schema version', () => {
      const histogram = createValidHistogram(5);
      histogram.schema_version = 'v999'; // Invalid version

      render(<CertifiedHistogram runId="run_test" data={histogram} />);

      expect(screen.getByText('DISTRIBUTION UNAVAILABLE')).toBeInTheDocument();
    });

    it('shows unavailable state when bins array is empty', () => {
      const histogram = createValidHistogram(5);
      histogram.bins = [];
      histogram.binning.bin_count = 0;
      histogram.total_samples = 0;

      render(<CertifiedHistogram runId="run_test" data={histogram} />);

      expect(screen.getByText(/DISTRIBUTION UNAVAILABLE/)).toBeInTheDocument();
    });

    it('shows loading state when no preloaded data is provided', () => {
      // Mock API to not resolve immediately
      mockApi.getWindowPnlHistogram.mockImplementation(() => new Promise(() => {}));
      
      render(<CertifiedHistogram runId="run_test" />);

      // Should show loading state while fetching
      expect(screen.getByText(/LOADING/)).toBeInTheDocument();
    });
  });

  describe('Fetching data', () => {
    it('fetches histogram when no preloaded data', async () => {
      const histogram = createValidHistogram(5);
      mockApi.getWindowPnlHistogram.mockResolvedValue(histogram);

      render(<CertifiedHistogram runId="run_fetch_test" binCount={50} />);

      expect(screen.getByText(/LOADING/)).toBeInTheDocument();

      await waitFor(() => {
        expect(mockApi.getWindowPnlHistogram).toHaveBeenCalledWith('run_fetch_test', 50);
      });

      await waitFor(() => {
        expect(screen.getByTestId('certified-histogram-svg')).toBeInTheDocument();
      });
    });

    it('shows error state on fetch failure', async () => {
      mockApi.getWindowPnlHistogram.mockRejectedValue(new Error('Network error'));

      render(<CertifiedHistogram runId="run_error_test" />);

      await waitFor(() => {
        expect(screen.getByText('DISTRIBUTION UNAVAILABLE')).toBeInTheDocument();
      });
    });

    it('has retry button on error', async () => {
      mockApi.getWindowPnlHistogram.mockRejectedValue(new Error('Network error'));

      render(<CertifiedHistogram runId="run_retry_test" />);

      await waitFor(() => {
        expect(screen.getByText('[RETRY]')).toBeInTheDocument();
      });
    });
  });

  describe('Locale independence', () => {
    it('formats USD values without locale-dependent separators', () => {
      const histogram: WindowPnlHistogramResponse = {
        schema_version: 'v1',
        run_id: 'run_test',
        manifest_hash: 'hash123',
        unit: 'USD',
        binning: { method: 'fixed_edges', bin_count: 3, min: -1234.56, max: 5678.90 },
        bins: [
          { left: -1234.56, right: 0, count: 5 },
          { left: 0, right: 2839.45, count: 15 },
          { left: 2839.45, right: 5678.90, count: 8 },
        ],
        underflow_count: 0,
        overflow_count: 0,
        total_samples: 28,
        trust_level: 'Trusted',
        is_trusted: true,
      };

      render(<CertifiedHistogram runId="run_test" data={histogram} />);

      // The min/max should be displayed with fixed precision, not locale-dependent
      expect(screen.getByText('-$1234.56')).toBeInTheDocument();
      expect(screen.getByText('$5678.90')).toBeInTheDocument();
    });
  });
});

describe('validateHistogramResponse', () => {
  it('returns null for valid histogram', () => {
    const histogram = createValidHistogram(10);
    expect(validateHistogramResponse(histogram)).toBeNull();
  });

  it('returns error for unsupported schema version', () => {
    const histogram = createValidHistogram(10);
    histogram.schema_version = 'v999';

    const error = validateHistogramResponse(histogram);
    expect(error).toContain('Unsupported schema version');
  });

  it('returns error for missing bins array', () => {
    const histogram = createValidHistogram(10);
    (histogram as any).bins = null;

    const error = validateHistogramResponse(histogram);
    expect(error).toContain('Missing bins array');
  });

  it('returns error for bin count mismatch', () => {
    const histogram = createValidHistogram(10);
    histogram.binning.bin_count = 999; // Wrong count

    const error = validateHistogramResponse(histogram);
    expect(error).toContain('Bin count mismatch');
  });

  it('returns error for non-contiguous bins', () => {
    const histogram: WindowPnlHistogramResponse = {
      schema_version: 'v1',
      run_id: 'run_test',
      manifest_hash: 'hash123',
      unit: 'USD',
      binning: { method: 'fixed_edges', bin_count: 2, min: 0, max: 20 },
      bins: [
        { left: 0, right: 10, count: 5 },
        { left: 15, right: 20, count: 8 }, // Gap! left should be 10
      ],
      underflow_count: 0,
      overflow_count: 0,
      total_samples: 13,
      trust_level: 'Trusted',
      is_trusted: true,
    };

    const error = validateHistogramResponse(histogram);
    expect(error).toContain('not contiguous');
  });

  it('returns error for count mismatch', () => {
    const histogram = createValidHistogram(5);
    histogram.total_samples = 99999; // Wrong total

    const error = validateHistogramResponse(histogram);
    expect(error).toContain('Count mismatch');
  });

  it('returns error for missing manifest_hash', () => {
    const histogram = createValidHistogram(5);
    histogram.manifest_hash = '';

    const error = validateHistogramResponse(histogram);
    expect(error).toContain('Missing manifest_hash');
  });
});

describe('Histogram determinism', () => {
  it('same preloaded data renders identically across re-renders', () => {
    const histogram = createValidHistogram(5);
    
    const { rerender } = render(<CertifiedHistogram runId="run_test" data={histogram} />);
    
    // Get initial bar attributes
    const initialBars = histogram.bins.map((_, i) => {
      const bar = screen.getByTestId(`histogram-bar-${i}`);
      return {
        left: bar.getAttribute('data-left'),
        right: bar.getAttribute('data-right'),
        count: bar.getAttribute('data-count'),
      };
    });

    // Re-render
    rerender(<CertifiedHistogram runId="run_test" data={histogram} />);

    // Check bars are identical
    histogram.bins.forEach((_, i) => {
      const bar = screen.getByTestId(`histogram-bar-${i}`);
      expect(bar.getAttribute('data-left')).toBe(initialBars[i].left);
      expect(bar.getAttribute('data-right')).toBe(initialBars[i].right);
      expect(bar.getAttribute('data-count')).toBe(initialBars[i].count);
    });
  });
});
