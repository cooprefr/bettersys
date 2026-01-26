import {Composition} from 'remotion';

import {
  tradeBetterPromoDefaultProps,
  TradeBetterPromo,
} from './compositions/TradeBetterPromo';

import {
  tradeBetterTerminalWalkthroughDefaultProps,
  TradeBetterTerminalWalkthrough,
} from './compositions/TradeBetterTerminalWalkthrough';

import {
  tradeBetterHyperPop10sDefaultProps,
  TradeBetterHyperPop10s,
} from './compositions/TradeBetterHyperPop10s';

import {
  TRADE_BETTER_POP_EDIT_10S_FRAMES,
  tradeBetterPopEdit10sDefaultProps,
  TradeBetterPopEdit10s,
} from './compositions/TradeBetterPopEdit10sV2';

export const RemotionRoot = () => {
  return (
    <>
      <Composition
        id="TradeBetterPromo"
        component={TradeBetterPromo}
        durationInFrames={600}
        fps={30}
        width={1080}
        height={1920}
        defaultProps={tradeBetterPromoDefaultProps}
      />

      <Composition
        id="TradeBetterTerminalWalkthrough"
        component={TradeBetterTerminalWalkthrough}
        durationInFrames={750}
        fps={30}
        width={1080}
        height={1920}
        defaultProps={tradeBetterTerminalWalkthroughDefaultProps}
      />

      <Composition
        id="TradeBetterHyperPop10s"
        component={TradeBetterHyperPop10s}
        durationInFrames={300}
        fps={30}
        width={1080}
        height={1920}
        defaultProps={tradeBetterHyperPop10sDefaultProps}
      />

      <Composition
        id="TradeBetterPopEdit10s"
        component={TradeBetterPopEdit10s}
        durationInFrames={TRADE_BETTER_POP_EDIT_10S_FRAMES}
        fps={30}
        width={1080}
        height={1920}
        defaultProps={tradeBetterPopEdit10sDefaultProps}
      />
    </>
  );
};
