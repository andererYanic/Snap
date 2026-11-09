// Testumgebung: native Teile der Animations-, Gesten- und Video-Bibliotheken nachbilden.
jest.mock('react-native-video', () => 'Video');

// Reanimated: Komponenten als normale Views, Animationen ohne Wirkung, Worklets direkt ausführen.
jest.mock('react-native-reanimated', () => {
  const RN = require('react-native');
  const anim = () => {
    const b = {
      duration: () => b, delay: () => b, springify: () => b, damping: () => b, stiffness: () => b, easing: () => b,
    };
    return b;
  };
  const names = ['FadeIn', 'FadeOut', 'FadeInDown', 'FadeOutDown', 'SlideInLeft', 'SlideInRight', 'ZoomIn', 'ZoomOut', 'LinearTransition'];
  const anims = Object.fromEntries(names.map((n) => [n, anim()]));
  const Animated = {
    View: RN.View, Text: RN.Text, FlatList: RN.FlatList, ScrollView: RN.ScrollView, Image: RN.Image,
    createAnimatedComponent: (c) => c,
  };
  return {
    __esModule: true,
    default: Animated,
    ...Animated,
    ...anims,
    useSharedValue: (v) => ({ value: v }),
    useAnimatedStyle: (f) => f(),
    withSpring: (v) => v,
    withTiming: (v, _c, cb) => { if (cb) { cb(true); } return v; },
    runOnJS: (f) => f,
    useEvent: () => () => {},
    useComposedEventHandler: (handlers) => (handlers && handlers.find(Boolean)) || (() => {}),
    useHandler: () => ({ context: {}, doDependenciesDiffer: false }),
    isSharedValue: () => false,
    Keyframe: class { constructor(d) { this.d = d; } duration() { return this; } delay() { return this; } },
    Easing: { bezier: () => (x) => x, out: (f) => f, in: (f) => f, cubic: (x) => x },
  };
});
jest.mock('react-native-worklets', () => ({ scheduleOnRN: (f, ...a) => f(...a) }));

require('react-native-gesture-handler/jestSetup');
