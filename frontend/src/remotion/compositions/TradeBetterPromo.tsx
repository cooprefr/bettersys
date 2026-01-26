import {
  AbsoluteFill,
  Easing,
  interpolate,
  Sequence,
  spring,
  useCurrentFrame,
  useVideoConfig,
} from 'remotion';

export type TradeBetterPromoProps = {
  brand: string;
  hook: string;
  tagline: string;
  bullets: string[];
  website: string;
  accentColor: string;
  backgroundColor: string;
};

export const tradeBetterPromoDefaultProps: TradeBetterPromoProps = {
  brand: 'TradeBetter',
  hook: 'FOLLOW THE WHALES.',
  tagline: 'Signal intelligence for prediction markets — in real time.',
  bullets: [
    'Real-time whale alerts',
    'Full-history search',
    'Performance dashboards',
    'Paper trading + backtests',
    'Fast API + low latency UX',
  ],
  website: 'tradebetter.xyz',
  accentColor: '#7CFFB2',
  backgroundColor: '#050508',
};

const FONT_SANS =
  '-apple-system, BlinkMacSystemFont, Segoe UI, Inter, Roboto, Helvetica, Arial, sans-serif';
const FONT_MONO =
  'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, Liberation Mono, monospace';

const TopBrand: React.FC<Pick<TradeBetterPromoProps, 'brand' | 'accentColor'>> = ({
  brand,
  accentColor,
}) => {
  const frame = useCurrentFrame();
  const opacity = interpolate(frame, [0, 16], [0, 1], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });

  return (
    <div
      style={{
        position: 'absolute',
        top: 70,
        left: 70,
        display: 'flex',
        flexDirection: 'column',
        gap: 10,
        opacity,
      }}
    >
      <div
        style={{
          fontFamily: FONT_MONO,
          fontSize: 18,
          letterSpacing: 2,
          color: 'rgba(255,255,255,0.78)',
        }}
      >
        {brand.toUpperCase()}
      </div>
      <div
        style={{
          height: 2,
          width: 140,
          borderRadius: 999,
          background: accentColor,
          boxShadow: `0 0 24px ${accentColor}44`,
        }}
      />
    </div>
  );
};

const HookCard: React.FC<Pick<TradeBetterPromoProps, 'hook' | 'tagline' | 'accentColor'>> = ({
  hook,
  tagline,
  accentColor,
}) => {
  const frame = useCurrentFrame();
  const {fps} = useVideoConfig();

  const enter = spring({
    frame,
    fps,
    config: {
      damping: 14,
      stiffness: 180,
      mass: 0.6,
    },
  });

  const opacity = interpolate(frame, [0, 10], [0, 1], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });

  const glitchX = frame < 16 ? Math.sin(frame * 0.9) * 3 : 0;
  const glitchY = frame < 16 ? Math.cos(frame * 1.1) * 2 : 0;

  return (
    <AbsoluteFill
      style={{
        justifyContent: 'center',
        padding: 90,
        opacity,
        transform: `translate(${glitchX}px, ${glitchY}px) translateY(${interpolate(
          enter,
          [0, 1],
          [54, 0],
        )}px)`,
      }}
    >
      <div style={{display: 'flex', flexDirection: 'column', gap: 20}}>
        <div
          style={{
            fontFamily: FONT_SANS,
            fontSize: 96,
            fontWeight: 900,
            letterSpacing: -2.0,
            lineHeight: 0.98,
            color: '#FFFFFF',
            textTransform: 'uppercase',
          }}
        >
          {hook}
        </div>
        <div
          style={{
            fontFamily: FONT_SANS,
            fontSize: 34,
            fontWeight: 600,
            lineHeight: 1.18,
            color: 'rgba(255,255,255,0.82)',
            maxWidth: 960,
          }}
        >
          {tagline}
        </div>
        <div style={{display: 'flex', alignItems: 'center', gap: 12, marginTop: 6}}>
          <div
            style={{
              fontFamily: FONT_MONO,
              fontSize: 13,
              letterSpacing: 1.8,
              padding: '8px 12px',
              borderRadius: 999,
              border: `1px solid ${accentColor}55`,
              background: 'rgba(255,255,255,0.03)',
              color: accentColor,
              boxShadow: `0 0 28px ${accentColor}22`,
            }}
          >
            LIVE
          </div>
          <div
            style={{
              fontFamily: FONT_MONO,
              fontSize: 13,
              letterSpacing: 0.6,
              color: 'rgba(255,255,255,0.62)',
            }}
          >
            Signals · Search · Dashboards
          </div>
        </div>
      </div>
    </AbsoluteFill>
  );
};

const BulletsCard: React.FC<Pick<TradeBetterPromoProps, 'bullets' | 'accentColor'>> = ({
  bullets,
  accentColor,
}) => {
  const globalFrame = useCurrentFrame();
  const frame = Math.max(0, globalFrame);

  const baseOpacity = interpolate(frame, [0, 10], [0, 1], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });

  return (
    <AbsoluteFill style={{justifyContent: 'center', padding: 90, opacity: baseOpacity}}>
      <div style={{display: 'flex', flexDirection: 'column', gap: 18}}>
        <div
          style={{
            fontFamily: FONT_SANS,
            fontSize: 52,
            fontWeight: 900,
            letterSpacing: -1.0,
            color: '#FFFFFF',
            marginBottom: 10,
          }}
        >
          Everything you need.
        </div>
        {bullets.slice(0, 6).map((b, i) => {
          const from = 8 + i * 10;
          const y = interpolate(frame, [from, from + 14], [26, 0], {
            extrapolateLeft: 'clamp',
            extrapolateRight: 'clamp',
          });
          const o = interpolate(frame, [from, from + 14], [0, 1], {
            extrapolateLeft: 'clamp',
            extrapolateRight: 'clamp',
          });

          return (
            <div
              key={`${b}-${i}`}
              style={{
                display: 'flex',
                gap: 14,
                alignItems: 'center',
                transform: `translateY(${y}px)`,
                opacity: o,
              }}
            >
              <div
                style={{
                  width: 10,
                  height: 10,
                  borderRadius: 999,
                  background: accentColor,
                  boxShadow: `0 0 18px ${accentColor}66`,
                  flex: '0 0 auto',
                }}
              />
              <div
                style={{
                  fontFamily: FONT_SANS,
                  fontSize: 40,
                  fontWeight: 700,
                  letterSpacing: -0.2,
                  lineHeight: 1.1,
                  color: 'rgba(255,255,255,0.86)',
                }}
              >
                {b}
              </div>
            </div>
          );
        })}
      </div>
    </AbsoluteFill>
  );
};

type DemoSignal = {
  market: string;
  side: 'BUY' | 'SELL';
  edgeBps: number;
  wallet: string;
  ageSec: number;
};

const demoSignals: DemoSignal[] = [
  {market: 'BTC UP 15M', side: 'BUY', edgeBps: 42.0, wallet: 'insider_crypto', ageSec: 2},
  {market: 'ETH UP 15M', side: 'BUY', edgeBps: 18.5, wallet: 'insider_crypto', ageSec: 4},
  {market: 'SOL DN 15M', side: 'SELL', edgeBps: -27.0, wallet: 'insider_other', ageSec: 7},
  {market: 'XRP UP 15M', side: 'BUY', edgeBps: 11.2, wallet: 'insider_finance', ageSec: 9},
  {market: 'BTC UP 15M', side: 'BUY', edgeBps: 33.8, wallet: 'insider_crypto', ageSec: 12},
  {market: 'ETH DN 15M', side: 'SELL', edgeBps: -14.7, wallet: 'insider_other', ageSec: 14},
  {market: 'SOL UP 15M', side: 'BUY', edgeBps: 21.1, wallet: 'insider_crypto', ageSec: 16},
  {market: 'XRP DN 15M', side: 'SELL', edgeBps: -9.4, wallet: 'insider_finance', ageSec: 19},
  {market: 'BTC UP 15M', side: 'BUY', edgeBps: 26.6, wallet: 'insider_crypto', ageSec: 22},
  {market: 'ETH UP 15M', side: 'BUY', edgeBps: 15.3, wallet: 'insider_crypto', ageSec: 25},
];

const TerminalDemo: React.FC<Pick<TradeBetterPromoProps, 'accentColor'>> = ({accentColor}) => {
  const frame = useCurrentFrame();
  const {fps} = useVideoConfig();

  const enter = spring({
    frame,
    fps,
    config: {
      damping: 16,
      stiffness: 140,
      mass: 0.8,
    },
  });

  const cardOpacity = interpolate(frame, [0, 10], [0, 1], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });

  const rowH = 62;
  const scroll = interpolate(frame, [0, 180], [0, -rowH * 4], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });

  const query = 'search: "whale btc up"';
  const typedLen = Math.max(0, Math.min(query.length, Math.floor(frame / 2.5)));
  const typed = query.slice(0, typedLen);

  const edgeColor = (edgeBps: number) => {
    if (edgeBps >= 20) return '#22c55e';
    if (edgeBps >= 8) return '#eab308';
    if (edgeBps <= -20) return '#ef4444';
    if (edgeBps <= -8) return '#f97316';
    return 'rgba(255,255,255,0.75)';
  };

  return (
    <AbsoluteFill
      style={{
        justifyContent: 'center',
        padding: 70,
        opacity: cardOpacity,
        transform: `translateY(${interpolate(enter, [0, 1], [40, 0])}px)`,
      }}
    >
      <div
        style={{
          display: 'flex',
          flexDirection: 'column',
          gap: 14,
          borderRadius: 18,
          border: '1px solid rgba(255,255,255,0.16)',
          background: 'rgba(0,0,0,0.35)',
          boxShadow: `0 0 70px ${accentColor}18`,
          overflow: 'hidden',
        }}
      >
        {/* Header */}
        <div
          style={{
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'space-between',
            padding: '18px 20px',
            borderBottom: '1px solid rgba(255,255,255,0.10)',
          }}
        >
          <div style={{display: 'flex', alignItems: 'center', gap: 12}}>
            <div
              style={{
                width: 9,
                height: 9,
                borderRadius: 99,
                background: accentColor,
                boxShadow: `0 0 18px ${accentColor}66`,
              }}
            />
            <div
              style={{
                fontFamily: FONT_MONO,
                fontSize: 12,
                letterSpacing: 1.2,
                color: 'rgba(255,255,255,0.70)',
                textTransform: 'uppercase',
              }}
            >
              Live Terminal
            </div>
          </div>
          <div
            style={{
              fontFamily: FONT_MONO,
              fontSize: 12,
              color: 'rgba(255,255,255,0.62)',
            }}
          >
            {typed}
            <span style={{opacity: frame % 20 < 10 ? 1 : 0.2}}>|</span>
          </div>
        </div>

        {/* Rows */}
        <div style={{position: 'relative', height: rowH * 5, overflow: 'hidden'}}>
          <div style={{transform: `translateY(${scroll}px)`}}>
            {demoSignals.map((s, i) => {
              const bg = i % 2 === 0 ? 'rgba(255,255,255,0.02)' : 'transparent';
              const sideColor = s.side === 'BUY' ? '#22c55e' : '#ef4444';
              const edge = `${s.edgeBps > 0 ? '+' : ''}${s.edgeBps.toFixed(1)}bps`;
              return (
                <div
                  key={`${s.market}-${i}`}
                  style={{
                    height: rowH,
                    display: 'flex',
                    alignItems: 'center',
                    justifyContent: 'space-between',
                    padding: '0 20px',
                    background: bg,
                    borderBottom: '1px solid rgba(255,255,255,0.08)',
                  }}
                >
                  <div style={{display: 'flex', alignItems: 'baseline', gap: 14}}>
                    <div style={{
                      fontFamily: FONT_MONO,
                      fontSize: 12,
                      color: 'rgba(255,255,255,0.55)',
                      width: 66,
                    }}>
                      {s.ageSec}s
                    </div>
                    <div
                      style={{
                        fontFamily: FONT_MONO,
                        fontSize: 18,
                        letterSpacing: 0.4,
                        color: 'rgba(255,255,255,0.90)',
                      }}
                    >
                      {s.market}
                    </div>
                    <div
                      style={{
                        fontFamily: FONT_MONO,
                        fontSize: 12,
                        letterSpacing: 1.4,
                        color: sideColor,
                        border: `1px solid ${sideColor}44`,
                        borderRadius: 999,
                        padding: '6px 10px',
                        background: 'rgba(0,0,0,0.25)',
                      }}
                    >
                      {s.side}
                    </div>
                  </div>
                  <div style={{display: 'flex', alignItems: 'baseline', gap: 14}}>
                    <div
                      style={{
                        fontFamily: FONT_MONO,
                        fontSize: 12,
                        color: 'rgba(255,255,255,0.55)',
                      }}
                    >
                      {s.wallet}
                    </div>
                    <div
                      style={{
                        fontFamily: FONT_MONO,
                        fontSize: 18,
                        fontWeight: 800,
                        letterSpacing: -0.2,
                        color: edgeColor(s.edgeBps),
                      }}
                    >
                      {edge}
                    </div>
                  </div>
                </div>
              );
            })}
          </div>
        </div>
      </div>
    </AbsoluteFill>
  );
};

const CtaCard: React.FC<Pick<TradeBetterPromoProps, 'website' | 'accentColor'>> = ({
  website,
  accentColor,
}) => {
  const frame = useCurrentFrame();

  const opacity = interpolate(frame, [0, 14], [0, 1], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });

  const pulse = interpolate(
    Math.sin((frame / 30) * Math.PI * 2),
    [-1, 1],
    [0.9, 1.0],
    {
      easing: Easing.inOut(Easing.quad),
    },
  );

  return (
    <AbsoluteFill style={{justifyContent: 'flex-end', padding: 90, opacity}}>
      <div style={{display: 'flex', flexDirection: 'column', gap: 18}}>
        <div
          style={{
            fontFamily: FONT_SANS,
            fontSize: 48,
            fontWeight: 800,
            letterSpacing: -0.8,
            color: '#FFFFFF',
          }}
        >
          Get the edge before the market moves.
        </div>
        <div
          style={{
            display: 'flex',
            alignItems: 'center',
            gap: 16,
            marginTop: 6,
          }}
        >
          <div
            style={{
              padding: '18px 24px',
              borderRadius: 16,
              border: `1px solid ${accentColor}55`,
              background: 'rgba(255,255,255,0.03)',
              boxShadow: `0 0 40px ${accentColor}22`,
              transform: `scale(${pulse})`,
            }}
          >
            <div
              style={{
                fontFamily: FONT_MONO,
                fontSize: 44,
                fontWeight: 800,
                letterSpacing: -0.6,
                color: accentColor,
              }}
            >
              {website}
            </div>
          </div>
        </div>
      </div>
    </AbsoluteFill>
  );
};

export const TradeBetterPromo: React.FC<TradeBetterPromoProps> = (props) => {
  const {brand, hook, tagline, bullets, website, accentColor, backgroundColor} = props;
  const frame = useCurrentFrame();

  const vignette = interpolate(frame, [0, 90], [0.60, 0.86], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });

  const scanlineOpacity = interpolate(Math.sin((frame / 30) * Math.PI * 2), [-1, 1], [0.03, 0.06], {
    easing: Easing.inOut(Easing.quad),
  });

  return (
    <AbsoluteFill
      style={{
        backgroundColor,
        fontFamily: FONT_SANS,
      }}
    >
      <AbsoluteFill
        style={{
          background:
            `radial-gradient(1200px 1200px at 50% 20%, ${accentColor}22 0%, transparent 55%),` +
            `radial-gradient(900px 900px at 20% 90%, ${accentColor}14 0%, transparent 55%)`,
        }}
      />
      <AbsoluteFill
        style={{
          background: `radial-gradient(1200px 1200px at 50% 50%, rgba(0,0,0,0) 0%, rgba(0,0,0,${vignette}) 100%)`,
        }}
      />
      <AbsoluteFill
        style={{
          background:
            'linear-gradient(transparent 0%, rgba(255,255,255,0.03) 50%, transparent 100%)',
          opacity: scanlineOpacity,
          transform: `translateY(${(frame * 6) % 900}px)`,
        }}
      />

      <TopBrand brand={brand} accentColor={accentColor} />

      {/* X-optimized vertical hype: 20s @ 30fps */}
      <Sequence from={0} durationInFrames={90}>
        <HookCard hook={hook} tagline={tagline} accentColor={accentColor} />
      </Sequence>

      <Sequence from={80} durationInFrames={210}>
        <BulletsCard bullets={bullets} accentColor={accentColor} />
      </Sequence>

      <Sequence from={270} durationInFrames={240}>
        <TerminalDemo accentColor={accentColor} />
      </Sequence>

      <Sequence from={470} durationInFrames={130}>
        <CtaCard website={website} accentColor={accentColor} />
      </Sequence>
    </AbsoluteFill>
  );
};
