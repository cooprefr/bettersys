import {
  AbsoluteFill,
  Audio,
  Easing,
  interpolate,
  Sequence,
  useCurrentFrame,
  useVideoConfig,
  Video,
} from 'remotion';

// eslint-disable-next-line @typescript-eslint/ban-ts-comment
// @ts-ignore
import blastoffVideo from '../input/BETTER_BLASTOFF.mp4';
// eslint-disable-next-line @typescript-eslint/ban-ts-comment
// @ts-ignore
import terminalRecording from '../input/recording_1.mp4';

export type TradeBetterHyperPop10sProps = {
  accentColor: string;
  backgroundColor: string;
};

export const tradeBetterHyperPop10sDefaultProps: TradeBetterHyperPop10sProps = {
  accentColor: '#7CFFB2',
  backgroundColor: '#050508',
};

const HOT_PINK = '#FF3DDB';
const CYAN = '#38BDF8';

const clipFrames = 10;

const clamp = (v: number) => Math.max(0, Math.min(1, v));
const fract = (v: number) => v - Math.floor(v);
const rnd = (seed: number) => fract(Math.sin(seed * 9999.123) * 43758.5453);

const Scanlines: React.FC<{opacity?: number}> = ({opacity = 0.08}) => {
  const frame = useCurrentFrame();
  const y = (frame * 6) % 240;
  return (
    <AbsoluteFill
      style={{
        pointerEvents: 'none',
        opacity,
        background:
          'repeating-linear-gradient(to bottom, rgba(255,255,255,0.06) 0px, rgba(255,255,255,0.06) 1px, rgba(0,0,0,0) 3px, rgba(0,0,0,0) 6px)',
        transform: `translateY(${y}px)`,
        mixBlendMode: 'overlay',
      }}
    />
  );
};

const Vignette: React.FC = () => {
  return (
    <AbsoluteFill
      style={{
        pointerEvents: 'none',
        background:
          'radial-gradient(1200px 1200px at 50% 40%, rgba(0,0,0,0) 0%, rgba(0,0,0,0.68) 62%, rgba(0,0,0,0.92) 100%)',
      }}
    />
  );
};

const CutFlash: React.FC = () => {
  const frame = useCurrentFrame();
  const p = frame % clipFrames;
  const opacity = interpolate(p, [0, 1, 3], [0.95, 0.35, 0], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
    easing: Easing.out(Easing.quad),
  });
  return (
    <AbsoluteFill
      style={{
        pointerEvents: 'none',
        opacity,
        background: '#FFFFFF',
        mixBlendMode: 'overlay',
      }}
    />
  );
};

const ClipBackground: React.FC<{
  src: 'terminal' | 'blastoff';
  startSec: number;
  hue: number;
  accentColor: string;
  intensity: number;
  clipIndex: number;
}> = ({src, startSec, hue, accentColor, intensity, clipIndex}) => {
  const frame = useCurrentFrame();
  const {fps} = useVideoConfig();

  const local = frame - clipIndex * clipFrames;
  const shakeX = (rnd(clipIndex * 9.1 + local * 0.7) - 0.5) * 16 * intensity;
  const shakeY = (rnd(clipIndex * 5.7 + local * 0.8) - 0.5) * 22 * intensity;
  const rot = (rnd(clipIndex * 13.3) - 0.5) * 2.2 * intensity;

  const t = clamp(local / (clipFrames - 1));
  const zoom = 1.15 + Math.sin((t + clipIndex * 0.3) * Math.PI * 2) * 0.03;

  const trimBefore = Math.round(startSec * fps);
  const trimAfter = trimBefore + Math.round(2.0 * fps);

  const source = src === 'terminal' ? terminalRecording : blastoffVideo;

  return (
    <AbsoluteFill
      style={{
        transform: `translate(${shakeX}px, ${shakeY}px) rotate(${rot}deg) scale(${zoom})`,
      }}
    >
      <Video
        src={source}
        trimBefore={trimBefore}
        trimAfter={trimAfter}
        muted
        style={{
          width: '100%',
          height: '100%',
          objectFit: 'cover',
          filter: `blur(10px) saturate(1.9) contrast(1.55) brightness(0.85) hue-rotate(${hue}deg)`,
        }}
      />

      <AbsoluteFill
        style={{
          background:
            `radial-gradient(900px 900px at 50% 20%, ${accentColor}30 0%, transparent 55%),` +
            `radial-gradient(900px 900px at 20% 85%, ${HOT_PINK}22 0%, transparent 62%)`,
          mixBlendMode: 'screen',
          opacity: 0.55,
        }}
      />

      <AbsoluteFill
        style={{
          background:
            'linear-gradient(180deg, rgba(255,255,255,0.10) 0%, rgba(255,255,255,0.00) 25%, rgba(0,0,0,0.20) 100%)',
          opacity: 0.7,
          mixBlendMode: 'overlay',
        }}
      />
    </AbsoluteFill>
  );
};

const CircuitOverlay: React.FC<{accentColor: string; clipIndex: number}> = ({
  accentColor,
  clipIndex,
}) => {
  const frame = useCurrentFrame();
  const {width, height} = useVideoConfig();
  const local = frame - clipIndex * clipFrames;
  const dash = interpolate(local, [0, clipFrames], [0, -220], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });

  return (
    <AbsoluteFill style={{pointerEvents: 'none', opacity: 0.9}}>
      <svg width={width} height={height} viewBox="0 0 1080 1920" preserveAspectRatio="none">
        <g fill="none" strokeLinecap="round" strokeLinejoin="round">
          <path
            d="M90 320 H620 V860 H980"
            stroke={accentColor}
            strokeWidth={5}
            strokeDasharray="18 14"
            strokeDashoffset={dash}
            opacity={0.85}
          />
          <path
            d="M180 1500 H900 V1200 H520 V640 H260"
            stroke={CYAN}
            strokeWidth={4}
            strokeDasharray="16 16"
            strokeDashoffset={dash * 1.1}
            opacity={0.8}
          />
          <path
            d="M120 1040 H460 V1180 H760 V980 H1040"
            stroke={HOT_PINK}
            strokeWidth={4}
            strokeDasharray="22 18"
            strokeDashoffset={dash * 0.9}
            opacity={0.75}
          />
          <circle cx="620" cy="860" r="10" fill={accentColor} opacity={0.9} />
          <circle cx="900" cy="1200" r="9" fill={CYAN} opacity={0.85} />
          <circle cx="760" cy="1180" r="9" fill={HOT_PINK} opacity={0.8} />
        </g>
      </svg>
    </AbsoluteFill>
  );
};

const OrderBookOverlay: React.FC<{accentColor: string; clipIndex: number}> = ({
  accentColor,
  clipIndex,
}) => {
  const frame = useCurrentFrame();
  const {width, height} = useVideoConfig();
  const local = frame - clipIndex * clipFrames;
  const cols = 26;

  return (
    <AbsoluteFill
      style={{
        pointerEvents: 'none',
        padding: 90,
        opacity: 0.95,
        mixBlendMode: 'screen',
      }}
    >
      <div
        style={{
          position: 'absolute',
          left: 90,
          right: 90,
          top: 140,
          bottom: 220,
          display: 'grid',
          gridTemplateColumns: `repeat(${cols}, 1fr)`,
          gap: 10,
          alignItems: 'end',
        }}
      >
        {new Array(cols).fill(true).map((_, i) => {
          const side = i < cols / 2 ? 'buy' : 'sell';
          const seed = i * 13.7 + clipIndex * 41.2;
          const osc = Math.sin((local * 0.85 + seed) * 0.9);
          const h = 0.22 + 0.78 * (0.5 + 0.5 * osc);
          const barH = h * (height - 420);
          const base = side === 'buy' ? accentColor : HOT_PINK;
          const glow = side === 'buy' ? `${accentColor}66` : `${HOT_PINK}66`;
          const opacity = 0.55 + 0.45 * (0.5 + 0.5 * Math.sin((local + i) * 1.3));
          return (
            <div
              key={i}
              style={{
                height: barH,
                borderRadius: 10,
                background: `linear-gradient(180deg, ${base} 0%, ${base}33 75%, transparent 100%)`,
                boxShadow: `0 0 26px ${glow}`,
                opacity,
              }}
            />
          );
        })}
      </div>
      <AbsoluteFill
        style={{
          background:
            'linear-gradient(90deg, rgba(255,255,255,0.06) 1px, transparent 1px), linear-gradient(rgba(255,255,255,0.06) 1px, transparent 1px)',
          backgroundSize: '60px 60px',
          opacity: 0.20,
          mixBlendMode: 'overlay',
        }}
      />
      <div
        style={{
          position: 'absolute',
          left: width / 2 - 3,
          top: 120,
          width: 6,
          bottom: 160,
          borderRadius: 999,
          background: 'rgba(255,255,255,0.18)',
          boxShadow: '0 0 30px rgba(255,255,255,0.16)',
          mixBlendMode: 'overlay',
        }}
      />
    </AbsoluteFill>
  );
};

const ChipOverlay: React.FC<{accentColor: string; clipIndex: number}> = ({
  accentColor,
  clipIndex,
}) => {
  const frame = useCurrentFrame();
  const {width, height} = useVideoConfig();
  const local = frame - clipIndex * clipFrames;
  const pulse = 0.5 + 0.5 * Math.sin((local + clipIndex) * 1.3);
  const glow = interpolate(pulse, [0, 1], [0.25, 0.95], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });

  const chipW = 640;
  const chipH = 460;
  const cx = width / 2;
  const cy = height / 2;
  const x = cx - chipW / 2;
  const y = cy - chipH / 2;

  const pins = 16;
  const pinLen = 26;
  const pinW = 10;

  return (
    <AbsoluteFill style={{pointerEvents: 'none', opacity: 0.95}}>
      <svg width={width} height={height} viewBox={`0 0 ${width} ${height}`}>
        <defs>
          <filter id="chipGlow" x="-20%" y="-20%" width="140%" height="140%">
            <feGaussianBlur stdDeviation="12" result="blur" />
            <feColorMatrix
              in="blur"
              type="matrix"
              values="1 0 0 0 0  0 1 0 0 0  0 0 1 0 0  0 0 0 18 -8"
              result="glow"
            />
            <feMerge>
              <feMergeNode in="glow" />
              <feMergeNode in="SourceGraphic" />
            </feMerge>
          </filter>
        </defs>

        {/* Pins */}
        {new Array(pins).fill(true).map((_, i) => {
          const t = (i + 1) / (pins + 1);
          const px = x + t * chipW;
          return (
            <g key={i} opacity={0.85}>
              <rect x={px - pinW / 2} y={y - pinLen} width={pinW} height={pinLen} fill={CYAN} />
              <rect x={px - pinW / 2} y={y + chipH} width={pinW} height={pinLen} fill={HOT_PINK} />
            </g>
          );
        })}
        {new Array(pins).fill(true).map((_, i) => {
          const t = (i + 1) / (pins + 1);
          const py = y + t * chipH;
          return (
            <g key={i} opacity={0.85}>
              <rect x={x - pinLen} y={py - pinW / 2} width={pinLen} height={pinW} fill={HOT_PINK} />
              <rect x={x + chipW} y={py - pinW / 2} width={pinLen} height={pinW} fill={CYAN} />
            </g>
          );
        })}

        {/* Chip body */}
        <rect
          x={x}
          y={y}
          width={chipW}
          height={chipH}
          rx={26}
          fill="rgba(0,0,0,0.35)"
          stroke={accentColor}
          strokeWidth={3}
          filter="url(#chipGlow)"
          opacity={0.9}
        />

        {/* Inner traces */}
        <g
          strokeLinecap="round"
          strokeLinejoin="round"
          fill="none"
          opacity={0.65 + 0.35 * glow}
        >
          <path
            d={`M ${x + 70} ${y + 120} H ${x + 300} V ${y + 230} H ${x + 560}`}
            stroke={CYAN}
            strokeWidth={4}
          />
          <path
            d={`M ${x + 80} ${y + 330} H ${x + 250} V ${y + 290} H ${x + 540}`}
            stroke={HOT_PINK}
            strokeWidth={4}
          />
          <path
            d={`M ${x + 150} ${y + 70} V ${y + 390}`}
            stroke={accentColor}
            strokeWidth={3}
            strokeDasharray="10 14"
            strokeDashoffset={-local * 18}
          />
        </g>
      </svg>
    </AbsoluteFill>
  );
};

const ProfitCurveOverlay: React.FC<{accentColor: string; clipIndex: number}> = ({
  accentColor,
  clipIndex,
}) => {
  const frame = useCurrentFrame();
  const {width, height} = useVideoConfig();
  const local = frame - clipIndex * clipFrames;

  const pad = 120;
  const x0 = pad;
  const y0 = height - pad;
  const x1 = width - pad;
  const y1 = pad;

  const pts = 10;
  const points = new Array(pts).fill(true).map((_, i) => {
    const t = i / (pts - 1);
    const x = x0 + t * (x1 - x0);
    const wobble = (rnd(clipIndex * 100 + i * 11) - 0.5) * 140;
    const y = y0 + (y1 - y0) * t + wobble * (1 - t) * 0.45;
    return `${x.toFixed(1)},${y.toFixed(1)}`;
  });

  const draw = interpolate(local, [0, clipFrames - 1], [1, 0], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
    easing: Easing.out(Easing.cubic),
  });
  const dash = 2600;
  const dashOffset = dash * draw;

  return (
    <AbsoluteFill style={{pointerEvents: 'none', opacity: 0.95}}>
      <svg width={width} height={height} viewBox={`0 0 ${width} ${height}`}>
        <polyline
          points={points.join(' ')}
          fill="none"
          stroke={accentColor}
          strokeWidth={10}
          strokeLinecap="round"
          strokeLinejoin="round"
          strokeDasharray={dash}
          strokeDashoffset={dashOffset}
          style={{filter: `drop-shadow(0 0 28px ${accentColor}88)`}}
        />
        <polyline
          points={points.join(' ')}
          fill="none"
          stroke={CYAN}
          strokeWidth={4}
          strokeLinecap="round"
          strokeLinejoin="round"
          strokeDasharray={dash}
          strokeDashoffset={dashOffset + 180}
          opacity={0.85}
        />
        <polyline
          points={points.join(' ')}
          fill="none"
          stroke={HOT_PINK}
          strokeWidth={4}
          strokeLinecap="round"
          strokeLinejoin="round"
          strokeDasharray={dash}
          strokeDashoffset={dashOffset + 360}
          opacity={0.75}
        />
      </svg>
      <AbsoluteFill
        style={{
          background:
            'linear-gradient(90deg, rgba(255,255,255,0.08) 1px, transparent 1px), linear-gradient(rgba(255,255,255,0.08) 1px, transparent 1px)',
          backgroundSize: '80px 80px',
          opacity: 0.18,
          mixBlendMode: 'overlay',
        }}
      />
    </AbsoluteFill>
  );
};

const ProbBarsOverlay: React.FC<{accentColor: string; clipIndex: number}> = ({
  accentColor,
  clipIndex,
}) => {
  const frame = useCurrentFrame();
  const {width} = useVideoConfig();
  const local = frame - clipIndex * clipFrames;

  const p = 0.14 + 0.72 * (0.5 + 0.5 * Math.sin((local + clipIndex * 3) * 0.85));
  const left = Math.round((width - 180) * p);

  const wobble = (rnd(clipIndex * 7.7 + local * 0.9) - 0.5) * 10;

  return (
    <AbsoluteFill
      style={{
        pointerEvents: 'none',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        opacity: 0.95,
        transform: `translateY(${wobble}px)`,
      }}
    >
      <div
        style={{
          position: 'relative',
          width: width - 180,
          height: 140,
          borderRadius: 18,
          overflow: 'hidden',
          border: '1px solid rgba(255,255,255,0.16)',
          boxShadow: '0 30px 110px rgba(0,0,0,0.55)',
          background: 'rgba(0,0,0,0.28)',
        }}
      >
        <div
          style={{
            width: left,
            height: '100%',
            background: `linear-gradient(90deg, ${accentColor} 0%, ${accentColor}66 70%, transparent 100%)`,
            boxShadow: `0 0 40px ${accentColor}66`,
            mixBlendMode: 'screen',
          }}
        />
        <div
          style={{
            position: 'absolute',
            top: 0,
            left,
            width: 6,
            height: '100%',
            borderRadius: 999,
            background: 'rgba(255,255,255,0.40)',
            boxShadow: '0 0 30px rgba(255,255,255,0.22)',
            opacity: 0.9,
          }}
        />
        <div
          style={{
            position: 'absolute',
            top: 0,
            left: left + 6,
            right: 0,
            height: '100%',
            background: `linear-gradient(90deg, ${HOT_PINK}99 0%, ${HOT_PINK}22 70%, transparent 100%)`,
            mixBlendMode: 'screen',
          }}
        />
      </div>
    </AbsoluteFill>
  );
};

type OverlayKind = 'circuit' | 'orderbook' | 'chip' | 'curve' | 'prob';

const Clip: React.FC<{
  clipIndex: number;
  overlay: OverlayKind;
  bgSrc: 'terminal' | 'blastoff';
  bgStartSec: number;
  accentColor: string;
}> = ({clipIndex, overlay, bgSrc, bgStartSec, accentColor}) => {
  const hue = (clipIndex * 37) % 360;
  const intensity = 0.8 + 0.2 * Math.sin(clipIndex * 0.7);

  return (
    <AbsoluteFill>
      <ClipBackground
        src={bgSrc}
        startSec={bgStartSec}
        hue={hue}
        accentColor={accentColor}
        intensity={intensity}
        clipIndex={clipIndex}
      />

      {overlay === 'circuit' && <CircuitOverlay accentColor={accentColor} clipIndex={clipIndex} />}
      {overlay === 'orderbook' && <OrderBookOverlay accentColor={accentColor} clipIndex={clipIndex} />}
      {overlay === 'chip' && <ChipOverlay accentColor={accentColor} clipIndex={clipIndex} />}
      {overlay === 'curve' && <ProfitCurveOverlay accentColor={accentColor} clipIndex={clipIndex} />}
      {overlay === 'prob' && <ProbBarsOverlay accentColor={accentColor} clipIndex={clipIndex} />}
    </AbsoluteFill>
  );
};

export const TradeBetterHyperPop10s: React.FC<TradeBetterHyperPop10sProps> = ({
  accentColor,
  backgroundColor,
}) => {
  const frame = useCurrentFrame();
  const {fps} = useVideoConfig();

  const clips = 30;

  const terminalTimes = [
    10.6, 11.8, 19.8, 22.4, 26.2, 34.8, 39.2, 44.0, 49.6, 54.2, 58.0, 64.9,
  ];
  const blastTimes = [0.2, 1.8, 3.4, 5.2, 7.0, 8.6, 10.4, 12.2, 14.0, 16.0, 18.0, 20.0];

  const overlayKinds: OverlayKind[] = ['circuit', 'orderbook', 'chip', 'curve', 'prob'];

  const audioStart = Math.round(7.0 * fps);
  const audioEnd = audioStart + clips * clipFrames;
  const audioVol = (f: number) => {
    const cut = f % clipFrames;
    const bump = interpolate(cut, [0, 1, 3], [0.24, 0.10, 0], {
      extrapolateLeft: 'clamp',
      extrapolateRight: 'clamp',
    });
    const fadeIn = interpolate(f, [0, 12], [0, 1], {
      extrapolateLeft: 'clamp',
      extrapolateRight: 'clamp',
    });
    const fadeOut = interpolate(f, [clips * clipFrames - 18, clips * clipFrames - 1], [1, 0], {
      extrapolateLeft: 'clamp',
      extrapolateRight: 'clamp',
    });
    return (0.22 + bump) * fadeIn * fadeOut;
  };

  return (
    <AbsoluteFill style={{backgroundColor}}>
      <Audio src={blastoffVideo} trimBefore={audioStart} trimAfter={audioEnd} volume={audioVol} />

      {new Array(clips).fill(true).map((_, i) => {
        const from = i * clipFrames;

        const useBlastoff = i % 6 === 0 || i % 6 === 1;
        const bgSrc: 'terminal' | 'blastoff' = useBlastoff ? 'blastoff' : 'terminal';
        const startSec = useBlastoff
          ? blastTimes[(i * 5) % blastTimes.length] + (i % 3) * 0.12
          : terminalTimes[(i * 7) % terminalTimes.length] + (i % 3) * 0.10;

        const overlay = overlayKinds[i % overlayKinds.length];

        return (
          <Sequence key={i} from={from} durationInFrames={clipFrames}>
            <Clip
              clipIndex={i}
              overlay={overlay}
              bgSrc={bgSrc}
              bgStartSec={startSec}
              accentColor={accentColor}
            />
          </Sequence>
        );
      })}

      <AbsoluteFill
        style={{
          pointerEvents: 'none',
          background:
            'linear-gradient(90deg, rgba(255,255,255,0.05) 1px, transparent 1px), linear-gradient(rgba(255,255,255,0.05) 1px, transparent 1px)',
          backgroundSize: '96px 96px',
          opacity: 0.16,
          mixBlendMode: 'overlay',
        }}
      />

      <CutFlash />
      <Scanlines opacity={0.08} />
      <Vignette />

      <AbsoluteFill
        style={{
          pointerEvents: 'none',
          opacity: 0.12,
          background:
            'radial-gradient(900px 900px at 55% 18%, rgba(255,255,255,0.22) 0%, transparent 60%)',
          mixBlendMode: 'overlay',
        }}
      />

      <AbsoluteFill
        style={{
          pointerEvents: 'none',
          opacity: 0.08 + 0.06 * (0.5 + 0.5 * Math.sin(frame * 0.35)),
          background:
            `linear-gradient(180deg, ${accentColor}1a 0%, transparent 40%, ${HOT_PINK}12 100%)`,
          mixBlendMode: 'screen',
        }}
      />
    </AbsoluteFill>
  );
};
