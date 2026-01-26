import {
  AbsoluteFill,
  Audio,
  Easing,
  interpolate,
  Sequence,
  spring,
  useCurrentFrame,
  useVideoConfig,
  Video,
} from 'remotion';

// Assets (use only files in src/remotion/input)
// eslint-disable-next-line @typescript-eslint/ban-ts-comment
// @ts-ignore
import blastoffVideo from '../input/BETTER_BLASTOFF.mp4';
// eslint-disable-next-line @typescript-eslint/ban-ts-comment
// @ts-ignore
import terminalRecording from '../input/recording_1.mp4';

export type TradeBetterTerminalWalkthroughProps = {
  brand: string;
  url: string;
  xHandle?: string;
  accentColor: string;
  backgroundColor: string;
  line1: string;
  line2: string;
  seriousLine: string;
  disclaimer?: string;
};

export const tradeBetterTerminalWalkthroughDefaultProps: TradeBetterTerminalWalkthroughProps = {
  brand: 'TradeBetter',
  url: 'tradebetter.app',
  xHandle: '',
  accentColor: '#7CFFB2',
  backgroundColor: '#050508',
  line1: 'Institutional-grade',
  line2: 'prediction market tooling.',
  seriousLine: 'Built for serious traders.',
  disclaimer: '',
};

const FONT_SANS =
  '-apple-system, BlinkMacSystemFont, Segoe UI, Inter, Roboto, Helvetica, Arial, sans-serif';
const FONT_MONO =
  'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, Liberation Mono, monospace';

const clamp = (v: number) => Math.max(0, Math.min(1, v));

const eased = (t: number) => Easing.out(Easing.cubic)(clamp(t));

const Faded: React.FC<{
  durationInFrames: number;
  fade?: number;
  children: React.ReactNode;
}> = ({ durationInFrames, fade = 12, children }) => {
  const frame = useCurrentFrame();
  const i = interpolate(frame, [0, fade], [0, 1], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });
  const o = interpolate(frame, [durationInFrames - fade, durationInFrames], [1, 0], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });
  const opacity = Math.min(i, o);
  return <AbsoluteFill style={{ opacity }}>{children}</AbsoluteFill>;
};

const SafeFrame: React.FC<{ children: React.ReactNode }> = ({ children }) => {
  const { width, height } = useVideoConfig();
  const inset = 72; // safe margins for 1:1 / 16:9 crops
  return (
    <div
      style={{
        position: 'absolute',
        inset,
        width: width - inset * 2,
        height: height - inset * 2,
      }}
    >
      {children}
    </div>
  );
};

const Scanlines: React.FC<{ opacity?: number }> = ({ opacity = 0.06 }) => {
  const frame = useCurrentFrame();
  const y = (frame * 4) % 180;
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
          'radial-gradient(1100px 1100px at 50% 40%, rgba(0,0,0,0) 0%, rgba(0,0,0,0.72) 65%, rgba(0,0,0,0.92) 100%)',
      }}
    />
  );
};

const FramedVideo: React.FC<{
  src: string;
  trimBefore: number;
  trimAfter: number;
  // Foreground crop controls
  zoom?: number;
  panX?: number;
  panY?: number;
  objectPosition?: string;
  showBrowserChrome?: boolean;
  url?: string;
  accentColor: string;
}> = ({
  src,
  trimBefore,
  trimAfter,
  zoom = 1,
  panX = 0,
  panY = 0,
  objectPosition = 'center',
  showBrowserChrome = false,
  url,
  accentColor,
}) => {
  const frame = useCurrentFrame();
  const { fps, width, height } = useVideoConfig();

  const enter = spring({
    frame,
    fps,
    config: {
      damping: 18,
      stiffness: 120,
      mass: 0.8,
    },
  });

  const windowW = Math.round(width * 0.94);
  const windowH = Math.round(windowW * (9 / 16));

  return (
    <AbsoluteFill>
      {/* Blurred background fill */}
      <Video
        src={src}
        trimBefore={trimBefore}
        trimAfter={trimAfter}
        muted
        style={{
          width: '100%',
          height: '100%',
          objectFit: 'cover',
          filter: 'blur(28px) saturate(0.9) brightness(0.65)',
          transform: `scale(1.15)`,
        }}
      />

      {/* Foreground window */}
      <div
        style={{
          position: 'absolute',
          left: (width - windowW) / 2,
          top: (height - windowH) / 2 + 40,
          width: windowW,
          height: windowH,
          borderRadius: 18,
          overflow: 'hidden',
          border: '1px solid rgba(255,255,255,0.14)',
          boxShadow: `0 30px 120px rgba(0,0,0,0.65), 0 0 60px ${accentColor}14`,
          transform: `translateY(${interpolate(enter, [0, 1], [24, 0])}px) scale(${interpolate(
            enter,
            [0, 1],
            [0.985, 1],
          )})`,
        }}
      >
        {showBrowserChrome && (
          <div
            style={{
              height: 44,
              background: 'rgba(0,0,0,0.55)',
              borderBottom: '1px solid rgba(255,255,255,0.10)',
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'space-between',
              padding: '0 14px',
              fontFamily: FONT_MONO,
              fontSize: 12,
              color: 'rgba(255,255,255,0.75)',
            }}
          >
            <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
              <div style={{ width: 10, height: 10, borderRadius: 99, background: '#ef4444' }} />
              <div style={{ width: 10, height: 10, borderRadius: 99, background: '#f59e0b' }} />
              <div style={{ width: 10, height: 10, borderRadius: 99, background: '#22c55e' }} />
            </div>
            <div
              style={{
                flex: 1,
                marginLeft: 14,
                marginRight: 14,
                height: 26,
                borderRadius: 10,
                border: '1px solid rgba(255,255,255,0.10)',
                background: 'rgba(0,0,0,0.35)',
                display: 'flex',
                alignItems: 'center',
                padding: '0 10px',
                overflow: 'hidden',
                whiteSpace: 'nowrap',
                textOverflow: 'ellipsis',
              }}
            >
              {url}
            </div>
            <div style={{ width: 42 }} />
          </div>
        )}

        <div style={{ position: 'relative', width: '100%', height: showBrowserChrome ? windowH - 44 : '100%' }}>
          <Video
            src={src}
            trimBefore={trimBefore}
            trimAfter={trimAfter}
            muted
            style={{
              width: '100%',
              height: '100%',
              objectFit: 'cover',
              objectPosition,
              transform: `translate(${panX}px, ${panY}px) scale(${zoom})`,
            }}
          />
        </div>
      </div>
    </AbsoluteFill>
  );
};

const CursorClick: React.FC<{
  moveUntil: number;
  clickAt: number;
  accentColor: string;
}> = ({ moveUntil, clickAt, accentColor }) => {
  const frame = useCurrentFrame();
  const { width, height } = useVideoConfig();

  const windowW = Math.round(width * 0.94);
  const windowH = Math.round(windowW * (9 / 16));
  const winLeft = (width - windowW) / 2;
  const winTop = (height - windowH) / 2 + 40;

  // Cursor path (relative to the window)
  const from = { x: winLeft + windowW * 0.68, y: winTop + windowH * 0.22 };
  const to = { x: winLeft + windowW * 0.15, y: winTop + windowH * 0.86 };

  const t = frame / Math.max(1, moveUntil);
  const tt = eased(t);

  const x = interpolate(tt, [0, 1], [from.x, to.x]);
  const y = interpolate(tt, [0, 1], [from.y, to.y]);

  const clickT = clamp((frame - clickAt) / 10);
  const clickScale = interpolate(clickT, [0, 0.35, 1], [1, 0.92, 1]);

  const ripple = clamp((frame - clickAt) / 16);
  const rippleScale = interpolate(ripple, [0, 1], [0.3, 2.6]);
  const rippleOpacity = interpolate(ripple, [0, 0.4, 1], [0, 0.35, 0], {
    easing: Easing.out(Easing.quad),
  });

  // Button highlight (approximate)
  const hl = {
    x: winLeft + windowW * 0.065,
    y: winTop + windowH * 0.825,
    w: windowW * 0.24,
    h: windowH * 0.12,
  };
  const hlOpacity = interpolate(frame, [clickAt - 18, clickAt - 4, clickAt + 8], [0, 1, 0.55], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });

  return (
    <>
      <div
        style={{
          position: 'absolute',
          left: hl.x,
          top: hl.y,
          width: hl.w,
          height: hl.h,
          borderRadius: 10,
          border: `1px solid ${accentColor}88`,
          boxShadow: `0 0 24px ${accentColor}33`,
          opacity: hlOpacity,
          pointerEvents: 'none',
        }}
      />
      <div
        style={{
          position: 'absolute',
          left: x,
          top: y,
          transform: `translate(-10px, -6px) scale(${clickScale})`,
          filter: 'drop-shadow(0 6px 10px rgba(0,0,0,0.55))',
          pointerEvents: 'none',
        }}
      >
        <svg width="34" height="34" viewBox="0 0 24 24" fill="none">
          <path
            d="M5 3L19 12L12.6 13.3L15.4 21L12.6 22L9.8 14.2L5 17V3Z"
            fill="#FFFFFF"
            opacity={0.95}
          />
        </svg>
      </div>
      <div
        style={{
          position: 'absolute',
          left: to.x,
          top: to.y,
          width: 18,
          height: 18,
          borderRadius: 99,
          border: `2px solid ${accentColor}`,
          transform: `translate(-9px, -9px) scale(${rippleScale})`,
          opacity: rippleOpacity,
          pointerEvents: 'none',
          boxShadow: `0 0 30px ${accentColor}44`,
        }}
      />
    </>
  );
};

const CenterHeadline: React.FC<{
  accentColor: string;
  line1: string;
  line2: string;
}> = ({ accentColor, line1, line2 }) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();

  const enter = spring({
    frame,
    fps,
    config: { damping: 20, stiffness: 160, mass: 0.8 },
  });

  return (
    <SafeFrame>
      <div
        style={{
          position: 'absolute',
          left: 0,
          right: 0,
          top: '44%',
          transform: `translateY(-50%) scale(${interpolate(enter, [0, 1], [0.98, 1])})`,
          textAlign: 'center',
        }}
      >
        <div
          style={{
            fontFamily: FONT_SANS,
            fontSize: 58,
            fontWeight: 900,
            letterSpacing: -1.2,
            color: '#FFFFFF',
            textTransform: 'uppercase',
          }}
        >
          {line1}
        </div>
        <div
          style={{
            fontFamily: FONT_SANS,
            fontSize: 54,
            fontWeight: 900,
            letterSpacing: -1.2,
            color: '#FFFFFF',
            textTransform: 'uppercase',
          }}
        >
          {line2}
        </div>
        <div
          style={{
            height: 3,
            width: 220,
            margin: '18px auto 0',
            borderRadius: 999,
            background: accentColor,
            boxShadow: `0 0 30px ${accentColor}55`,
          }}
        />
      </div>
    </SafeFrame>
  );
};

const MinimalLine: React.FC<{ text: string }> = ({ text }) => {
  return (
    <SafeFrame>
      <div
        style={{
          position: 'absolute',
          left: 0,
          right: 0,
          top: '18%',
          textAlign: 'center',
        }}
      >
        <div
          style={{
            fontFamily: FONT_SANS,
            fontSize: 54,
            fontWeight: 900,
            letterSpacing: -1.2,
            color: '#FFFFFF',
          }}
        >
          {text}
        </div>
      </div>
    </SafeFrame>
  );
};

const EndCard: React.FC<{
  brand: string;
  url: string;
  xHandle?: string;
  disclaimer?: string;
  accentColor: string;
}> = ({ brand, url, xHandle, disclaimer, accentColor }) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();

  const enter = spring({ frame, fps, config: { damping: 18, stiffness: 130, mass: 0.9 } });

  const handle = (xHandle || '').trim();
  const showHandle = handle.length > 0;
  const showDisclaimer = (disclaimer || '').trim().length > 0;

  return (
    <SafeFrame>
      <div
        style={{
          position: 'absolute',
          left: 0,
          right: 0,
          top: '44%',
          transform: `translateY(-50%) scale(${interpolate(enter, [0, 1], [0.98, 1])})`,
          textAlign: 'center',
        }}
      >
        <div
          style={{
            fontFamily: FONT_SANS,
            fontSize: 62,
            fontWeight: 900,
            letterSpacing: -1.4,
            color: '#FFFFFF',
          }}
        >
          {brand}
        </div>
        <div
          style={{
            fontFamily: FONT_MONO,
            fontSize: 34,
            fontWeight: 800,
            letterSpacing: -0.4,
            color: accentColor,
            marginTop: 14,
          }}
        >
          {url}
        </div>
        {showHandle && (
          <div
            style={{
              fontFamily: FONT_MONO,
              fontSize: 18,
              color: 'rgba(255,255,255,0.68)',
              marginTop: 10,
            }}
          >
            {handle}
          </div>
        )}
        {showDisclaimer && (
          <div
            style={{
              fontFamily: FONT_MONO,
              fontSize: 12,
              color: 'rgba(255,255,255,0.40)',
              marginTop: 16,
            }}
          >
            {disclaimer}
          </div>
        )}
      </div>
    </SafeFrame>
  );
};

export const TradeBetterTerminalWalkthrough: React.FC<TradeBetterTerminalWalkthroughProps> = (props) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();

  const {
    brand,
    url,
    xHandle,
    accentColor,
    backgroundColor,
    line1,
    line2,
    seriousLine,
    disclaimer,
  } = props;

  const t0 = 0;
  const t1 = Math.round(3 * fps);
  const t2 = Math.round(8 * fps);
  const t3 = Math.round(15 * fps);
  const t4 = Math.round(22 * fps);
  const t5 = Math.round(25 * fps);

  // Background audio from the website clip (trim to 25s)
  const audioStart = Math.round(7.0 * fps);
  const audioEnd = audioStart + (t5 - t0);
  const clickFrame = Math.round(2.35 * fps);
  const audioVol = (f: number) => {
    const base = 0.24;
    const bump = interpolate(f, [clickFrame - 2, clickFrame, clickFrame + 10], [0, 0.18, 0], {
      extrapolateLeft: 'clamp',
      extrapolateRight: 'clamp',
    });
    const fadeOut = interpolate(f, [t4 - 40, t4 + 10], [1, 0.0], {
      extrapolateLeft: 'clamp',
      extrapolateRight: 'clamp',
    });
    return (base + bump) * fadeOut;
  };

  // Shot timings (with small overlaps for smooth crossfades)
  const s1 = { start: t0, end: t1 };
  const s2 = { start: t1 - 6, end: t2 };

  const a = { start: t2 - 6, end: Math.round(10 * fps) };
  const b = { start: a.end - 6, end: Math.round(12 * fps) };
  const c = { start: b.end - 6, end: Math.round(13.5 * fps) };
  const d = { start: c.end - 6, end: t3 };

  const e = { start: t3 - 6, end: Math.round(18.5 * fps) };
  const f = { start: e.end - 6, end: t4 };

  const endCard = { start: t4 - 10, end: t5 };

  const headlineFrom = Math.round(3.25 * fps);
  const headlineTo = Math.round(7.7 * fps);
  const seriousFrom = Math.round(15.2 * fps);
  const seriousTo = Math.round(18.2 * fps);

  const baseGlow = interpolate(Math.sin((frame / fps) * Math.PI * 2), [-1, 1], [0.06, 0.10], {
    easing: Easing.inOut(Easing.quad),
  });

  return (
    <AbsoluteFill style={{ backgroundColor, fontFamily: FONT_SANS }}>
      <AbsoluteFill
        style={{
          background:
            `radial-gradient(900px 900px at 50% 18%, ${accentColor}1f 0%, transparent 58%),` +
            `radial-gradient(850px 850px at 18% 86%, ${accentColor}12 0%, transparent 62%)`,
          opacity: baseGlow,
        }}
      />

      <Audio src={blastoffVideo} trimBefore={audioStart} trimAfter={audioEnd} volume={audioVol} />

      {/* 0-3s: Website in browser view + mouse click */}
      <Sequence from={s1.start} durationInFrames={s1.end - s1.start}>
        <Faded durationInFrames={s1.end - s1.start} fade={12}>
          <FramedVideo
            src={blastoffVideo}
            trimBefore={Math.round(7.0 * fps)}
            trimAfter={Math.round(10.2 * fps)}
            showBrowserChrome
            url={`https://${url}`}
            accentColor={accentColor}
            zoom={1.02}
            panX={-8}
            panY={-6}
            objectPosition="left center"
          />
          <CursorClick
            moveUntil={Math.round(2.0 * fps)}
            clickAt={clickFrame}
            accentColor={accentColor}
          />
        </Faded>
      </Sequence>

      {/* 3-8s: Terminal launching + headline */}
      <Sequence from={s2.start} durationInFrames={s2.end - s2.start}>
        <Faded durationInFrames={s2.end - s2.start} fade={14}>
          <FramedVideo
            src={terminalRecording}
            trimBefore={Math.round(10.6 * fps)}
            trimAfter={Math.round(15.8 * fps)}
            accentColor={accentColor}
            zoom={1.10}
            panY={-70}
            objectPosition="center"
          />
        </Faded>
      </Sequence>

      <Sequence from={headlineFrom} durationInFrames={headlineTo - headlineFrom}>
        <Faded durationInFrames={headlineTo - headlineFrom} fade={10}>
          <CenterHeadline accentColor={accentColor} line1={line1} line2={line2} />
        </Faded>
      </Sequence>

      {/* 8-15s: Rapid but readable cuts */}
      <Sequence from={a.start} durationInFrames={a.end - a.start}>
        <Faded durationInFrames={a.end - a.start} fade={10}>
          <FramedVideo
            src={terminalRecording}
            trimBefore={Math.round(11.8 * fps)}
            trimAfter={Math.round(13.8 * fps)}
            accentColor={accentColor}
            zoom={1.10}
            panY={-70}
          />
        </Faded>
      </Sequence>

      <Sequence from={b.start} durationInFrames={b.end - b.start}>
        <Faded durationInFrames={b.end - b.start} fade={10}>
          <FramedVideo
            src={terminalRecording}
            trimBefore={Math.round(19.8 * fps)}
            trimAfter={Math.round(21.8 * fps)}
            accentColor={accentColor}
            zoom={1.14}
            panX={-110}
            panY={-70}
          />
        </Faded>
      </Sequence>

      <Sequence from={c.start} durationInFrames={c.end - c.start}>
        <Faded durationInFrames={c.end - c.start} fade={10}>
          <FramedVideo
            src={terminalRecording}
            trimBefore={Math.round(49.6 * fps)}
            trimAfter={Math.round(51.2 * fps)}
            accentColor={accentColor}
            zoom={1.12}
            panY={-58}
          />
        </Faded>
      </Sequence>

      <Sequence from={d.start} durationInFrames={d.end - d.start}>
        <Faded durationInFrames={d.end - d.start} fade={10}>
          <FramedVideo
            src={terminalRecording}
            trimBefore={Math.round(64.9 * fps)}
            trimAfter={Math.round(66.4 * fps)}
            accentColor={accentColor}
            zoom={1.10}
            panY={-66}
          />
        </Faded>
      </Sequence>

      {/* 15-22s: Slower, let shots breathe */}
      <Sequence from={e.start} durationInFrames={e.end - e.start}>
        <Faded durationInFrames={e.end - e.start} fade={14}>
          <FramedVideo
            src={terminalRecording}
            trimBefore={Math.round(64.9 * fps)}
            trimAfter={Math.round(68.7 * fps)}
            accentColor={accentColor}
            zoom={1.08}
            panY={-66}
          />
        </Faded>
      </Sequence>

      <Sequence from={f.start} durationInFrames={f.end - f.start}>
        <Faded durationInFrames={f.end - f.start} fade={14}>
          <FramedVideo
            src={terminalRecording}
            trimBefore={Math.round(34.8 * fps)}
            trimAfter={Math.round(39.0 * fps)}
            accentColor={accentColor}
            zoom={1.12}
            panX={-90}
            panY={-62}
          />
        </Faded>
      </Sequence>

      <Sequence from={seriousFrom} durationInFrames={seriousTo - seriousFrom}>
        <Faded durationInFrames={seriousTo - seriousFrom} fade={12}>
          <MinimalLine text={seriousLine} />
        </Faded>
      </Sequence>

      {/* 22-25s: End frame */}
      <Sequence from={endCard.start} durationInFrames={endCard.end - endCard.start}>
        <Faded durationInFrames={endCard.end - endCard.start} fade={12}>
          <FramedVideo
            src={blastoffVideo}
            trimBefore={Math.round(0.2 * fps)}
            trimAfter={Math.round(3.2 * fps)}
            accentColor={accentColor}
            zoom={1.02}
            objectPosition="center"
          />
        </Faded>
      </Sequence>

      <Sequence from={t4} durationInFrames={t5 - t4}>
        <Faded durationInFrames={t5 - t4} fade={12}>
          <EndCard
            brand={brand}
            url={url}
            xHandle={xHandle}
            disclaimer={disclaimer}
            accentColor={accentColor}
          />
        </Faded>
      </Sequence>

      <Scanlines opacity={0.06} />
      <Vignette />
    </AbsoluteFill>
  );
};
