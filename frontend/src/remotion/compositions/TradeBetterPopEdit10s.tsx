import {
  AbsoluteFill,
  Audio,
  Easing,
  Img,
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

import chalkboardImg from '../input/popedit/chalkboard.jpg';
import circuit1Img from '../input/popedit/circuit1.jpg';
import circuit2Img from '../input/popedit/circuit2.jpg';
import mathPaperImg from '../input/popedit/math_paper.jpg';
import serverRackImg from '../input/popedit/server_rack.jpg';
import stockChartImg from '../input/popedit/stock_chart.jpg';
import tbSite01Img from '../input/popedit/tb_site_01.jpg';
import tbSite02Img from '../input/popedit/tb_site_02.jpg';
import tbTerminal01Img from '../input/popedit/tb_terminal_01.jpg';
import tbTerminal02Img from '../input/popedit/tb_terminal_02.jpg';
import tbTerminal03Img from '../input/popedit/tb_terminal_03.jpg';
import tbTerminal04Img from '../input/popedit/tb_terminal_04.jpg';

export type TradeBetterPopEdit10sProps = {
  accentColor: string;
  backgroundColor: string;
};

export const tradeBetterPopEdit10sDefaultProps: TradeBetterPopEdit10sProps = {
  accentColor: '#7CFFB2',
  backgroundColor: '#050508',
};

const HOT_PINK = '#FF3DDB';
const CYAN = '#38BDF8';

const CLIP_FRAMES = 8;
const CLIPS = 37;
export const TRADE_BETTER_POP_EDIT_10S_FRAMES = CLIP_FRAMES * CLIPS; // 296 (~9.9s)

const clamp = (v: number) => Math.max(0, Math.min(1, v));
const fract = (v: number) => v - Math.floor(v);
const rnd = (seed: number) => fract(Math.sin(seed * 9999.123) * 43758.5453);

const CutFlash: React.FC = () => {
  const frame = useCurrentFrame();
  const p = frame % CLIP_FRAMES;
  const opacity = interpolate(p, [0, 1, 3], [0.95, 0.28, 0], {
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
          'radial-gradient(1200px 1200px at 50% 40%, rgba(0,0,0,0) 0%, rgba(0,0,0,0.70) 62%, rgba(0,0,0,0.92) 100%)',
      }}
    />
  );
};

type ShotSpec =
  | {
      kind: 'image';
      src: string;
      fit: 'contain' | 'cover';
    }
  | {
      kind: 'video';
      src: 'blastoff' | 'terminal';
      startSec: number;
      fit: 'contain' | 'cover';
    };

const ContentBox: React.FC<{
  children: React.ReactNode;
  clipIndex: number;
  accentColor: string;
}> = ({children, clipIndex, accentColor}) => {
  const frame = useCurrentFrame();
  const {width, height} = useVideoConfig();

  const local = frame - clipIndex * CLIP_FRAMES;
  const t = clamp(local / Math.max(1, CLIP_FRAMES - 1));
  const e = Easing.out(Easing.cubic)(t);

  const boxW = width;
  const boxH = Math.round(height * 0.63);
  const top = Math.round((height - boxH) / 2);

  const zoom = interpolate(e, [0, 1], [1.04, 1.14]);
  const rot = (rnd(clipIndex * 13.1) - 0.5) * 2.2;
  const dx = (rnd(clipIndex * 7.7) - 0.5) * 40;
  const dy = (rnd(clipIndex * 9.3) - 0.5) * 50;

  // Cut smear/glitch at the beginning of each clip
  const glitch = interpolate(local, [0, 1, 3], [1, 0.35, 0], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
    easing: Easing.out(Easing.quad),
  });

  const hue = (clipIndex * 34) % 360;
  const filter = `blur(${glitch * 12}px) saturate(${1.15 + glitch * 1.4}) contrast(${1.2 + glitch * 1.6}) brightness(${0.95 + glitch * 0.12}) hue-rotate(${hue}deg)`;

  return (
    <div
      style={{
        position: 'absolute',
        left: 0,
        top,
        width: boxW,
        height: boxH,
        transform: `translate(${dx}px, ${dy}px) rotate(${rot}deg) scale(${zoom})`,
        transformOrigin: '50% 50%',
        borderRadius: 22,
        overflow: 'hidden',
        border: '1px solid rgba(255,255,255,0.14)',
        background: 'rgba(0,0,0,0.86)',
        boxShadow: `0 40px 130px rgba(0,0,0,0.72), 0 0 70px ${accentColor}12`,
        filter,
      }}
    >
      {children}

      {/* neon edge glow */}
      <AbsoluteFill
        style={{
          pointerEvents: 'none',
          background:
            `radial-gradient(900px 900px at 55% 18%, ${accentColor}26 0%, transparent 58%),` +
            `radial-gradient(900px 900px at 22% 85%, ${HOT_PINK}1f 0%, transparent 62%)`,
          opacity: 0.75,
          mixBlendMode: 'screen',
        }}
      />

      {/* subtle grid */}
      <AbsoluteFill
        style={{
          pointerEvents: 'none',
          background:
            'linear-gradient(90deg, rgba(255,255,255,0.06) 1px, transparent 1px), linear-gradient(rgba(255,255,255,0.06) 1px, transparent 1px)',
          backgroundSize: '76px 76px',
          opacity: 0.18,
          mixBlendMode: 'overlay',
        }}
      />
    </div>
  );
};

const Shot: React.FC<{
  clipIndex: number;
  spec: ShotSpec;
  accentColor: string;
}> = ({clipIndex, spec, accentColor}) => {
  const frame = useCurrentFrame();
  const {fps} = useVideoConfig();
  const local = frame - clipIndex * CLIP_FRAMES;

  // slight rhythmic pulse per clip
  const pulse = 0.5 + 0.5 * Math.sin((local + clipIndex * 2) * 0.8);
  const glow = interpolate(pulse, [0, 1], [0.12, 0.22]);
  const showRgb = spec.kind === 'image' && local < 3;

  const mediaStyle: React.CSSProperties = {
    width: '100%',
    height: '100%',
    objectFit: spec.fit,
    transform: `scale(${1.02 + glow})`,
  };

  return (
    <>
      <ContentBox clipIndex={clipIndex} accentColor={accentColor}>
        {spec.kind === 'image' ? (
          <>
            <Img src={spec.src} style={mediaStyle} />
            {showRgb && (
              <>
                <Img
                  src={spec.src}
                  style={{
                    ...mediaStyle,
                    position: 'absolute',
                    inset: 0,
                    transform: `translate(10px, -6px) scale(${1.02 + glow})`,
                    opacity: 0.42,
                    filter: 'saturate(2.0) contrast(1.5) hue-rotate(160deg)',
                    mixBlendMode: 'screen',
                  }}
                />
                <Img
                  src={spec.src}
                  style={{
                    ...mediaStyle,
                    position: 'absolute',
                    inset: 0,
                    transform: `translate(-10px, 8px) scale(${1.02 + glow})`,
                    opacity: 0.38,
                    filter: 'saturate(2.2) contrast(1.6) hue-rotate(320deg)',
                    mixBlendMode: 'screen',
                  }}
                />
              </>
            )}
          </>
        ) : (
          <Video
            src={spec.src === 'blastoff' ? blastoffVideo : terminalRecording}
            trimBefore={Math.round(spec.startSec * fps)}
            trimAfter={Math.round(spec.startSec * fps) + Math.round(2.0 * fps)}
            muted
            style={mediaStyle}
          />
        )}

        {/* extra pop overlays */}
        <AbsoluteFill
          style={{
            pointerEvents: 'none',
            background:
              `linear-gradient(180deg, rgba(255,255,255,0.10) 0%, rgba(255,255,255,0.00) 26%, rgba(0,0,0,0.20) 100%)`,
            opacity: 0.65,
            mixBlendMode: 'overlay',
          }}
        />

        <AbsoluteFill
          style={{
            pointerEvents: 'none',
            opacity: 0.10 + glow,
            background:
              `linear-gradient(90deg, ${accentColor}1a 0%, transparent 40%, ${CYAN}12 70%, ${HOT_PINK}12 100%)`,
            mixBlendMode: 'screen',
          }}
        />
      </ContentBox>
    </>
  );
};

export const TradeBetterPopEdit10s: React.FC<TradeBetterPopEdit10sProps> = ({
  accentColor,
  backgroundColor,
}) => {
  const frame = useCurrentFrame();
  const {fps} = useVideoConfig();

  const stills = [
    tbSite01Img,
    tbTerminal01Img,
    chalkboardImg,
    mathPaperImg,
    circuit1Img,
    serverRackImg,
    tbTerminal02Img,
    stockChartImg,
    circuit2Img,
    tbTerminal03Img,
    tbSite02Img,
    tbTerminal04Img,
  ];

  const terminalTimes = [
    10.6, 11.8, 19.8, 22.4, 26.2, 34.8, 39.2, 44.0, 49.6, 54.2, 58.0, 64.9,
  ];
  const blastTimes = [0.2, 1.2, 2.3, 3.4, 5.2, 7.0, 8.6, 10.4, 12.2, 14.0, 16.0, 18.0];

  const specs: ShotSpec[] = new Array(CLIPS).fill(true).map((_, i) => {
    const useVideo = i % 9 === 0 || i % 9 === 4;
    const fit: 'contain' | 'cover' = i % 5 === 0 ? 'cover' : 'contain';

    if (useVideo) {
      const isBlast = i % 18 === 0;
      const startSec = isBlast
        ? blastTimes[(i * 5) % blastTimes.length] + (i % 3) * 0.10
        : terminalTimes[(i * 7) % terminalTimes.length] + (i % 3) * 0.08;
      return {
        kind: 'video',
        src: isBlast ? 'blastoff' : 'terminal',
        startSec,
        fit,
      };
    }

    return {
      kind: 'image',
      src: stills[i % stills.length],
      fit,
    };
  });

  const audioStart = Math.round(7.0 * fps);
  const audioEnd = audioStart + TRADE_BETTER_POP_EDIT_10S_FRAMES;
  const audioVol = (f: number) => {
    const cut = f % CLIP_FRAMES;
    const bump = interpolate(cut, [0, 1, 3], [0.26, 0.10, 0], {
      extrapolateLeft: 'clamp',
      extrapolateRight: 'clamp',
    });
    const fadeIn = interpolate(f, [0, 10], [0, 1], {
      extrapolateLeft: 'clamp',
      extrapolateRight: 'clamp',
    });
    const fadeOut = interpolate(f, [TRADE_BETTER_POP_EDIT_10S_FRAMES - 18, TRADE_BETTER_POP_EDIT_10S_FRAMES - 1], [1, 0], {
      extrapolateLeft: 'clamp',
      extrapolateRight: 'clamp',
    });
    return (0.20 + bump) * fadeIn * fadeOut;
  };

  return (
    <AbsoluteFill style={{backgroundColor}}>
      <AbsoluteFill
        style={{
          background:
            `radial-gradient(950px 950px at 50% 18%, ${accentColor}18 0%, transparent 60%),` +
            `radial-gradient(950px 950px at 18% 86%, ${HOT_PINK}12 0%, transparent 64%)`,
          opacity: 0.22,
        }}
      />

      <Audio src={blastoffVideo} trimBefore={audioStart} trimAfter={audioEnd} volume={audioVol} />

      {specs.map((spec, i) => (
        <Sequence key={i} from={i * CLIP_FRAMES} durationInFrames={CLIP_FRAMES}>
          <Shot clipIndex={i} spec={spec} accentColor={accentColor} />
        </Sequence>
      ))}

      {/* Ambient overlays */}
      <AbsoluteFill
        style={{
          pointerEvents: 'none',
          background:
            'linear-gradient(90deg, rgba(255,255,255,0.05) 1px, transparent 1px), linear-gradient(rgba(255,255,255,0.05) 1px, transparent 1px)',
          backgroundSize: '96px 96px',
          opacity: 0.14,
          mixBlendMode: 'overlay',
        }}
      />

      <CutFlash />
      <Scanlines opacity={0.08} />
      <Vignette />

      <AbsoluteFill
        style={{
          pointerEvents: 'none',
          opacity: 0.07 + 0.06 * (0.5 + 0.5 * Math.sin(frame * 0.35)),
          background:
            `linear-gradient(180deg, ${accentColor}14 0%, transparent 40%, ${HOT_PINK}12 100%)`,
          mixBlendMode: 'screen',
        }}
      />
    </AbsoluteFill>
  );
};
