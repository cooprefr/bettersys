import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import {
  CertifiedRunFooter,
  FooterMissingFieldsError,
  validateFooterData,
  type CertifiedRunFooterData,
} from '../CertifiedRunFooter';

// Mock the copyToClipboard function
vi.mock('../../../hooks/useRouter', () => ({
  copyToClipboard: vi.fn().mockResolvedValue(true),
}));

describe('CertifiedRunFooter', () => {
  const validProps = {
    runId: 'run_123456789abcdef0',
    publishTimestamp: 1706140800, // 2024-01-25T00:00:00Z
    schemaVersion: 'v1',
    manifestHash: 'abcdef1234567890abcdef1234567890abcdef12',
  };

  describe('Footer rendering', () => {
    it('renders all required fields on desktop', () => {
      render(<CertifiedRunFooter {...validProps} />);

      // Check data-testid elements
      expect(screen.getByTestId('certified-run-footer')).toBeInTheDocument();
      expect(screen.getByTestId('footer-run-id')).toHaveTextContent(validProps.runId);
      expect(screen.getByTestId('footer-schema-version')).toHaveTextContent(validProps.schemaVersion);
      
      // Check for manifest hash (truncated by default)
      const hashElement = screen.getByTestId('footer-manifest-hash');
      expect(hashElement).toBeInTheDocument();
    });

    it('renders footer with correct ARIA attributes', () => {
      render(<CertifiedRunFooter {...validProps} />);

      const footer = screen.getByTestId('certified-run-footer');
      expect(footer).toHaveAttribute('role', 'contentinfo');
      expect(footer).toHaveAttribute('aria-label', 'Certified run provenance information');
    });

    it('displays timestamp in UTC format', () => {
      render(<CertifiedRunFooter {...validProps} />);

      const timestampElement = screen.getByTestId('footer-publish-timestamp');
      expect(timestampElement.textContent).toContain('UTC');
    });

    it('displays truncated manifest hash by default', () => {
      render(<CertifiedRunFooter {...validProps} />);

      // The hash should be truncated to 12 characters
      const truncatedHash = validProps.manifestHash.slice(0, 12);
      expect(screen.getByText(truncatedHash)).toBeInTheDocument();
    });
  });

  describe('Manifest hash interaction', () => {
    it('shows full hash on hover', async () => {
      render(<CertifiedRunFooter {...validProps} />);

      const truncatedHash = validProps.manifestHash.slice(0, 12);
      const hashButton = screen.getByText(truncatedHash);

      fireEvent.mouseEnter(hashButton);

      await waitFor(() => {
        expect(screen.getByText(validProps.manifestHash)).toBeInTheDocument();
      });
    });

    it('shows full hash on click', async () => {
      render(<CertifiedRunFooter {...validProps} />);

      const truncatedHash = validProps.manifestHash.slice(0, 12);
      const hashButton = screen.getByText(truncatedHash);

      fireEvent.click(hashButton);

      await waitFor(() => {
        expect(screen.getByText(validProps.manifestHash)).toBeInTheDocument();
      });
    });

    it('has copy button for manifest hash', () => {
      render(<CertifiedRunFooter {...validProps} />);

      const copyButton = screen.getByRole('button', { name: /copy manifest hash/i });
      expect(copyButton).toBeInTheDocument();
    });
  });

  describe('Footer values change with different run_id', () => {
    it('updates all values when props change', () => {
      const { rerender } = render(<CertifiedRunFooter {...validProps} />);

      expect(screen.getByTestId('footer-run-id')).toHaveTextContent(validProps.runId);

      const newProps = {
        runId: 'run_different123456',
        publishTimestamp: 1706227200,
        schemaVersion: 'v2',
        manifestHash: '1111111122222222333333334444444455555555',
      };

      rerender(<CertifiedRunFooter {...newProps} />);

      expect(screen.getByTestId('footer-run-id')).toHaveTextContent(newProps.runId);
      expect(screen.getByTestId('footer-schema-version')).toHaveTextContent(newProps.schemaVersion);
    });
  });

  describe('Footer remains unchanged across renders', () => {
    it('maintains values on re-render without prop changes', () => {
      const { rerender } = render(<CertifiedRunFooter {...validProps} />);

      const initialRunId = screen.getByTestId('footer-run-id').textContent;
      const initialSchemaVersion = screen.getByTestId('footer-schema-version').textContent;

      // Re-render with same props
      rerender(<CertifiedRunFooter {...validProps} />);

      expect(screen.getByTestId('footer-run-id').textContent).toBe(initialRunId);
      expect(screen.getByTestId('footer-schema-version').textContent).toBe(initialSchemaVersion);
    });
  });
});

describe('validateFooterData', () => {
  it('returns valid for complete data', () => {
    const data: CertifiedRunFooterData = {
      run_id: 'run_123',
      publish_timestamp: 1706140800,
      schema_version: 'v1',
      manifest_hash: 'abc123',
    };

    const result = validateFooterData(data);

    expect(result.isValid).toBe(true);
    expect(result.missingFields).toHaveLength(0);
  });

  it('returns invalid when run_id is null', () => {
    const data: CertifiedRunFooterData = {
      run_id: null,
      publish_timestamp: 1706140800,
      schema_version: 'v1',
      manifest_hash: 'abc123',
    };

    const result = validateFooterData(data);

    expect(result.isValid).toBe(false);
    expect(result.missingFields).toContain('run_id');
  });

  it('returns invalid when run_id is empty string', () => {
    const data: CertifiedRunFooterData = {
      run_id: '',
      publish_timestamp: 1706140800,
      schema_version: 'v1',
      manifest_hash: 'abc123',
    };

    const result = validateFooterData(data);

    expect(result.isValid).toBe(false);
    expect(result.missingFields).toContain('run_id');
  });

  it('returns invalid when publish_timestamp is null', () => {
    const data: CertifiedRunFooterData = {
      run_id: 'run_123',
      publish_timestamp: null,
      schema_version: 'v1',
      manifest_hash: 'abc123',
    };

    const result = validateFooterData(data);

    expect(result.isValid).toBe(false);
    expect(result.missingFields).toContain('publish_timestamp');
  });

  it('returns invalid when schema_version is undefined', () => {
    const data: CertifiedRunFooterData = {
      run_id: 'run_123',
      publish_timestamp: 1706140800,
      schema_version: undefined,
      manifest_hash: 'abc123',
    };

    const result = validateFooterData(data);

    expect(result.isValid).toBe(false);
    expect(result.missingFields).toContain('schema_version');
  });

  it('returns invalid when manifest_hash is missing', () => {
    const data: CertifiedRunFooterData = {
      run_id: 'run_123',
      publish_timestamp: 1706140800,
      schema_version: 'v1',
      manifest_hash: null,
    };

    const result = validateFooterData(data);

    expect(result.isValid).toBe(false);
    expect(result.missingFields).toContain('manifest_hash');
  });

  it('returns multiple missing fields when several are null', () => {
    const data: CertifiedRunFooterData = {
      run_id: null,
      publish_timestamp: undefined,
      schema_version: '',
      manifest_hash: null,
    };

    const result = validateFooterData(data);

    expect(result.isValid).toBe(false);
    expect(result.missingFields).toHaveLength(4);
    expect(result.missingFields).toContain('run_id');
    expect(result.missingFields).toContain('publish_timestamp');
    expect(result.missingFields).toContain('schema_version');
    expect(result.missingFields).toContain('manifest_hash');
  });
});

describe('FooterMissingFieldsError', () => {
  it('renders error state with missing fields', () => {
    render(<FooterMissingFieldsError missingFields={['run_id', 'manifest_hash']} />);

    expect(screen.getByRole('alert')).toBeInTheDocument();
    expect(screen.getByText('ARTIFACT PROVENANCE ERROR')).toBeInTheDocument();
    expect(screen.getByText('run_id')).toBeInTheDocument();
    expect(screen.getByText('manifest_hash')).toBeInTheDocument();
  });

  it('has correct ARIA attributes for accessibility', () => {
    render(<FooterMissingFieldsError missingFields={['schema_version']} />);

    const alert = screen.getByRole('alert');
    expect(alert).toHaveAttribute('aria-live', 'assertive');
  });

  it('lists all missing fields', () => {
    const missingFields = ['run_id', 'publish_timestamp', 'schema_version', 'manifest_hash'];
    render(<FooterMissingFieldsError missingFields={missingFields} />);

    missingFields.forEach((field) => {
      expect(screen.getByText(field)).toBeInTheDocument();
    });
  });
});

describe('Footer integration with API response', () => {
  it('validates footer data matches expected API response shape', () => {
    // Simulate API response shape
    const apiResponse = {
      run_id: 'run_test123',
      manifest_hash: 'hash123abc456def',
      schema_version: 'v1',
      publish_timestamp: 1706140800,
      created_at: 1706140800,
    };

    const footerData: CertifiedRunFooterData = {
      run_id: apiResponse.run_id,
      manifest_hash: apiResponse.manifest_hash,
      schema_version: apiResponse.schema_version,
      publish_timestamp: apiResponse.publish_timestamp ?? apiResponse.created_at,
    };

    const result = validateFooterData(footerData);

    expect(result.isValid).toBe(true);
  });

  it('validates footer data handles fallback to created_at', () => {
    // Simulate API response without publish_timestamp (uses created_at)
    const apiResponse = {
      run_id: 'run_test123',
      manifest_hash: 'hash123abc456def',
      schema_version: 'v1',
      publish_timestamp: undefined,
      created_at: 1706140800,
    };

    const footerData: CertifiedRunFooterData = {
      run_id: apiResponse.run_id,
      manifest_hash: apiResponse.manifest_hash,
      schema_version: apiResponse.schema_version,
      publish_timestamp: apiResponse.publish_timestamp ?? apiResponse.created_at,
    };

    const result = validateFooterData(footerData);

    expect(result.isValid).toBe(true);
  });
});
