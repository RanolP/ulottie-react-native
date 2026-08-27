import {
  codegenNativeComponent,
  type CodegenTypes,
  type ViewProps,
} from 'react-native';

export interface NativeProps extends ViewProps {
  /**
   * JS-allocated identity of this view in the native registry — the handle
   * `global.UlottieRtApi.renderFrame(nativeId, frame)` addresses. 0 means
   * unassigned. Named `surfaceId` because RN core already claims the raw prop
   * name `nativeId` (the string form of JS `nativeID`) — an Int32 prop under
   * that name lands in `BaseViewProps.nativeId` and crashes consumers that
   * parse it as a string (reanimated's layout-animation proxy stoi's it).
   */
  surfaceId?: CodegenTypes.Int32;
}

export default codegenNativeComponent<NativeProps>('UlottieRtView');
