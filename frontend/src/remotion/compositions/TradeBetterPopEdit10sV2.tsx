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
import popeditSong from '../input/popeditex1.mp4';
// Old screenshots (8.xx pm)
import useShot085135 from '../input/popedit/use/Screenshot 2026-01-22 at 8.51.35\u202Fpm.png';
import useShot085446 from '../input/popedit/use/Screenshot 2026-01-22 at 8.54.46\u202Fpm.png';
import useShot085556 from '../input/popedit/use/Screenshot 2026-01-22 at 8.55.56\u202Fpm.png';
import useShot085636 from '../input/popedit/use/Screenshot 2026-01-22 at 8.56.36\u202Fpm.png';
import useShot085859 from '../input/popedit/use/Screenshot 2026-01-22 at 8.58.59\u202Fpm.png';
import useShot085945 from '../input/popedit/use/Screenshot 2026-01-22 at 8.59.45\u202Fpm.png';
import useShot090219 from '../input/popedit/use/Screenshot 2026-01-22 at 9.02.19\u202Fpm.png';
import useShot090447 from '../input/popedit/use/Screenshot 2026-01-22 at 9.04.47\u202Fpm.png';

// New screenshots (11.xx pm)
import useShot110245 from '../input/popedit/use/Screenshot 2026-01-22 at 11.02.45\u202Fpm.png';
import useShot110519 from '../input/popedit/use/Screenshot 2026-01-22 at 11.05.19\u202Fpm.png';
import useShot110620 from '../input/popedit/use/Screenshot 2026-01-22 at 11.06.20\u202Fpm.png';
import useShot110713 from '../input/popedit/use/Screenshot 2026-01-22 at 11.07.13\u202Fpm.png';
import useShot110847 from '../input/popedit/use/Screenshot 2026-01-22 at 11.08.47\u202Fpm.png';
import useShot110942 from '../input/popedit/use/Screenshot 2026-01-22 at 11.09.42\u202Fpm.png';
import useShot111023 from '../input/popedit/use/Screenshot 2026-01-22 at 11.10.23\u202Fpm.png';
import useShot111207 from '../input/popedit/use/Screenshot 2026-01-22 at 11.12.07\u202Fpm.png';
import useShot111348 from '../input/popedit/use/Screenshot 2026-01-22 at 11.13.48\u202Fpm.png';
import useShot111418 from '../input/popedit/use/Screenshot 2026-01-22 at 11.14.18\u202Fpm.png';
import useShot111645 from '../input/popedit/use/Screenshot 2026-01-22 at 11.16.45\u202Fpm.png';

import useOneJpg from '../input/popedit/use/1.jpg';
import useFinish from '../input/popedit/use/FINISH.jpg';

export type TradeBetterPopEdit10sProps = {
  accentColor: string;
  backgroundColor: string;
};

export const tradeBetterPopEdit10sDefaultProps: TradeBetterPopEdit10sProps = {
  accentColor: '#7CFFB2',
  backgroundColor: '#050508',
};

const HOT_PINK = '#FF3DDB';

// Full 10 seconds like popeditex1.mp4 (~9.83s @ 30fps = 295 frames)
export const TRADE_BETTER_POP_EDIT_10S_FRAMES = 295;

const INTRO_FRAMES = 106;      // ~3.53s - 1.jpg held at start
const MONTAGE_CLIP_FRAMES = 8; // Fast 8-frame cuts

const fract = (v: number) => v - Math.floor(v);
const rnd = (seed: number) => fract(Math.sin(seed * 9999.123) * 43758.5453);

const Scanlines: React.FC<{opacity?: number}> = ({opacity = 0.06}) => {
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
          'radial-gradient(1200px 1200px at 50% 40%, rgba(0,0,0,0) 0%, rgba(0,0,0,0.72) 62%, rgba(0,0,0,0.92) 100%)',
      }}
    />
  );
};

const MemeHold: React.FC<{src: string; accentColor: string; durationInFrames: number}> = ({
  src,
  accentColor,
  durationInFrames,
}) => {
  const frame = useCurrentFrame();
  const fade = interpolate(frame, [0, 4], [0, 1], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
    easing: Easing.out(Easing.cubic),
  });
  // Hype builds throughout the meme hold
  const hype = interpolate(frame, [6, durationInFrames - 1], [0, 1], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
    easing: Easing.inOut(Easing.quad),
  });
  const pulse = 0.5 + 0.5 * Math.sin(frame * 0.95);
  const hypeFlash = hype * (0.08 + 0.22 * pulse);

  return (
    <AbsoluteFill>
      <Img
        src={src}
        style={{
          position: 'absolute',
          inset: 0,
          width: '100%',
          height: '100%',
          objectFit: 'cover',
          transform: 'scale(1.12)',
          filter: 'blur(28px) saturate(1.35) contrast(1.1) brightness(0.70)',
          opacity: fade,
        }}
      />
      <Img
        src={src}
        style={{
          width: '100%',
          height: '100%',
          objectFit: 'contain',
          filter: 'contrast(1.06) saturate(1.14)',
          opacity: fade,
        }}
      />

      <AbsoluteFill
        style={{
          pointerEvents: 'none',
          opacity: hypeFlash,
          background: '#FFFFFF',
          mixBlendMode: 'overlay',
        }}
      />

      <AbsoluteFill
        style={{
          pointerEvents: 'none',
          opacity: 0.18 * hype,
          background:
            `radial-gradient(900px 900px at 50% 18%, ${accentColor}20 0%, transparent 58%),` +
            `radial-gradient(900px 900px at 18% 86%, ${HOT_PINK}18 0%, transparent 62%)`,
          mixBlendMode: 'screen',
        }}
      />
    </AbsoluteFill>
  );
};

type MontageSpec = {src: string; boost?: number; isVideo?: boolean};

// Different transition types for variety
const TRANSITIONS = ['slide', 'scale', 'fade', 'rotate', 'zoom'] as const;
type TransitionType = typeof TRANSITIONS[number];

const PopCard: React.FC<{
  spec: MontageSpec;
  clipIndex: number;
  durationInFrames: number;
  accentColor: string;
  transitionType?: TransitionType;
  intensity?: number; // 0-1, how flashy/energetic (decays over montage)
}> = ({spec, clipIndex, durationInFrames, accentColor, transitionType, intensity = 1}) => {
  const frame = useCurrentFrame();
  const {width, height} = useVideoConfig();

  const local = frame;
  const transition = transitionType ?? TRANSITIONS[clipIndex % TRANSITIONS.length];

  // Different entry animations based on transition type
  let tx = 0, ty = 0, rot = 0, scale = 1, opacity = 1;
  const prog = interpolate(local, [0, 3], [0, 1], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
    easing: Easing.out(Easing.cubic),
  });

  switch (transition) {
    case 'slide': {
      const dir = clipIndex % 4;
      const fromX = dir === 0 ? -200 : dir === 1 ? 200 : 0;
      const fromY = dir === 2 ? -250 : dir === 3 ? 250 : 0;
      tx = interpolate(prog, [0, 1], [fromX, 0]);
      ty = interpolate(prog, [0, 1], [fromY, 0]);
      break;
    }
    case 'scale': {
      scale = interpolate(prog, [0, 1], [0.3, 1]);
      opacity = interpolate(prog, [0, 1], [0, 1]);
      break;
    }
    case 'fade': {
      opacity = interpolate(prog, [0, 1], [0, 1]);
      scale = interpolate(prog, [0, 1], [1.1, 1]);
      break;
    }
    case 'rotate': {
      const fromRot = (clipIndex % 2 === 0 ? -1 : 1) * 15;
      rot = interpolate(prog, [0, 1], [fromRot, 0]);
      scale = interpolate(prog, [0, 1], [0.8, 1]);
      opacity = interpolate(prog, [0, 1], [0.5, 1]);
      break;
    }
    case 'zoom': {
      scale = interpolate(prog, [0, 1], [1.5, 1]);
      opacity = interpolate(prog, [0, 1], [0, 1]);
      break;
    }
  }

  // Jitter for energy (scaled by intensity)
  const jitter = interpolate(local, [0, 2], [1, 0], {extrapolateRight: 'clamp'}) * intensity;
  const jx = (rnd(clipIndex * 10.1 + local * 1.3) - 0.5) * 12 * jitter;
  const jy = (rnd(clipIndex * 5.7 + local * 1.7) - 0.5) * 16 * jitter;

  const cardW = Math.round(width * 0.96);
  const cardH = Math.round(height * 0.82);
  const left = Math.round((width - cardW) / 2);
  const top = Math.round((height - cardH) / 2);

  // Flash on entry (scaled by intensity)
  const flash = interpolate(local, [0, 1, 3], [0.7, 0.15, 0], {extrapolateRight: 'clamp'}) * intensity;

  const out = interpolate(local, [durationInFrames - 2, durationInFrames], [1, 0.9], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });

  const boost = spec.boost ?? 0;

  const bgStyle: React.CSSProperties = {
    width: '100%',
    height: '100%',
    objectFit: 'cover',
    transform: `scale(${1.16 + boost})`,
    filter: 'blur(28px) saturate(1.35) contrast(1.1) brightness(0.68)',
  };

  const fgStyle: React.CSSProperties = {
    width: '100%',
    height: '100%',
    objectFit: 'contain',
    transform: `scale(${1.02 + boost})`,
    filter: 'contrast(1.08) saturate(1.18)',
  };

  return (
    <AbsoluteFill style={{opacity: opacity * out}}>
      <div
        style={{
          position: 'absolute',
          left,
          top,
          width: cardW,
          height: cardH,
          background: 'rgba(0,0,0,0.92)',
          border: '1px solid rgba(255,255,255,0.14)',
          borderRadius: 26,
          overflow: 'hidden',
          boxShadow: `0 44px 150px rgba(0,0,0,0.72), 0 0 70px ${accentColor}18`,
          transform: `translate(${tx + jx}px, ${ty + jy}px) rotate(${rot}deg) scale(${scale})`,
          transformOrigin: '50% 50%',
        }}
      >
        {spec.isVideo ? (
          <Video
            src={spec.src}
            muted
            style={{...fgStyle, objectFit: 'cover'}}
            startFrom={0}
          />
        ) : (
          <>
            <Img src={spec.src} style={{...bgStyle, position: 'absolute', inset: 0, opacity: 0.95}} />
            <Img src={spec.src} style={fgStyle} />
          </>
        )}

        <AbsoluteFill style={{pointerEvents: 'none', opacity: flash, background: '#FFFFFF', mixBlendMode: 'overlay'}} />
        <AbsoluteFill
          style={{
            pointerEvents: 'none',
            opacity: 0.22,
            background:
              `radial-gradient(900px 900px at 50% 16%, ${accentColor}22 0%, transparent 58%),` +
              `radial-gradient(900px 900px at 22% 84%, ${HOT_PINK}1a 0%, transparent 62%)`,
            mixBlendMode: 'screen',
          }}
        />
      </div>
    </AbsoluteFill>
  );
};

// Fade in from black at the very start
const FadeFromBlack: React.FC = () => {
  const frame = useCurrentFrame();
  const opacity = interpolate(frame, [0, 18], [1, 0], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
    easing: Easing.out(Easing.cubic),
  });
  return (
    <AbsoluteFill
      style={{
        pointerEvents: 'none',
        background: '#000000',
        opacity,
        zIndex: 100,
      }}
    />
  );
};

export const TradeBetterPopEdit10s: React.FC<TradeBetterPopEdit10sProps> = ({
  accentColor,
  backgroundColor,
}) => {
  // Timeline (full 10 seconds = 295 frames):
  // 0-108f (3.6s): 1.jpg intro with fade from black + hype buildup
  // 108-295f (~6.23s): Fast 8-frame montage (19 images) + FINISH.jpg

  const montageFrom = INTRO_FRAMES;

  // All 19 images
  const montageSpecs: MontageSpec[] = [
    {src: useShot085135},
    {src: useShot110245},
    {src: useShot085446},
    {src: useShot110519},
    {src: useShot085556},
    {src: useShot110620},
    {src: useShot085636},
    {src: useShot110713},
    {src: useShot085859},
    {src: useShot110847},
    {src: useShot085945},
    {src: useShot110942},
    {src: useShot090219},
    {src: useShot111023},
    {src: useShot090447},
    {src: useShot111207},
    {src: useShot111348},
    {src: useShot111418},
    {src: useShot111645},
    // FINISH.jpg handled separately
  ];

  // All images in one continuous montage
  const totalMontageFrames = montageSpecs.length * MONTAGE_CLIP_FRAMES; // 19*8 = 152
  const finishStart = totalMontageFrames;
  // FINISH.jpg holds until the very end (when music fades out)
  const finishDuration = TRADE_BETTER_POP_EDIT_10S_FRAMES - montageFrom - finishStart;

  return (
    <AbsoluteFill style={{backgroundColor}}>
      <Audio
        src={popeditSong}
        trimBefore={0}
        trimAfter={TRADE_BETTER_POP_EDIT_10S_FRAMES}
        volume={(f) =>
          interpolate(f, [TRADE_BETTER_POP_EDIT_10S_FRAMES - 45, TRADE_BETTER_POP_EDIT_10S_FRAMES], [1, 0], {
            extrapolateLeft: 'clamp',
            extrapolateRight: 'clamp',
          })
        }
      />

      {/* Intro: 1.jpg for 3.5 seconds with fade from black + hype buildup */}
      <Sequence from={0} durationInFrames={INTRO_FRAMES}>
        <MemeHold src={useOneJpg} accentColor={accentColor} durationInFrames={INTRO_FRAMES} />
        <FadeFromBlack />
      </Sequence>

      {/* Montage section */}
      <Sequence from={montageFrom} durationInFrames={TRADE_BETTER_POP_EDIT_10S_FRAMES - montageFrom}>
        <AbsoluteFill>
          {/* All 19 images in continuous montage */}
          {montageSpecs.map((spec, i) => {
            const from = i * MONTAGE_CLIP_FRAMES;
            // Intensity: super flashy for first ~2 sec (7-8 clips), then cool down
            // First 8 clips = full intensity, then decay to 0.15 by end
            const intensity = i < 8 ? 1 : interpolate(i, [8, montageSpecs.length - 1], [0.7, 0.15], {extrapolateRight: 'clamp'});
            return (
              <Sequence key={i} from={from} durationInFrames={MONTAGE_CLIP_FRAMES}>
                <PopCard spec={spec} clipIndex={i} durationInFrames={MONTAGE_CLIP_FRAMES} accentColor={accentColor} intensity={intensity} />
              </Sequence>
            );
          })}

          {/* FINISH.jpg holds until music fades out - calm, no flash */}
          <Sequence from={finishStart} durationInFrames={finishDuration}>
            <PopCard spec={{src: useFinish}} clipIndex={montageSpecs.length} durationInFrames={finishDuration} accentColor={accentColor} transitionType="fade" intensity={0} />
          </Sequence>
        </AbsoluteFill>
      </Sequence>

      <AbsoluteFill
        style={{
          pointerEvents: 'none',
          background:
            'linear-gradient(90deg, rgba(255,255,255,0.05) 1px, transparent 1px), linear-gradient(rgba(255,255,255,0.05) 1px, transparent 1px)',
          backgroundSize: '110px 110px',
          opacity: 0.10,
          mixBlendMode: 'overlay',
        }}
      />

      <Scanlines opacity={0.06} />
      <Vignette />
    </AbsoluteFill>
  );
};
