import React from 'react';

interface ManifestLinkProps {
  manifestUrl: string;
  runId?: string;
  className?: string;
}

export const ManifestLink: React.FC<ManifestLinkProps> = ({
  manifestUrl,
  runId,
  className,
}) => {
  return (
    <a
      href={manifestUrl}
      target="_blank"
      rel="noopener noreferrer"
      className={`manifest-link inline-flex items-center gap-2 px-3 py-1.5 border border-grey/30 hover:border-accent bg-surface hover:bg-accent/10 font-mono text-xs tracking-widest text-fg/80 hover:text-accent transition-colors ${className ?? ''}`}
      data-testid="manifest-link"
      data-run-id={runId}
      title={`View immutable run manifest${runId ? ` for ${runId}` : ''}`}
    >
      <span className="manifest-icon" aria-hidden="true">
        📄
      </span>
      <span className="manifest-label">VIEW RUN MANIFEST</span>
    </a>
  );
};
