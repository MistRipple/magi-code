declare module 'react' {
  export type FC<P = {}> = (props: P) => any;
  export function useState<T>(initialState: T | (() => T)): [T, (newState: T) => void];
  export function useEffect(effect: () => void | (() => void), deps?: any[]): void;
  export function useRef<T>(initialValue: T | null): { current: T | null };
  export type ChangeEvent<T = Element> = { target: T & { files?: FileList } };
  export type DragEvent = { preventDefault: () => void; dataTransfer: { files: FileList } };
  export type MouseEvent = { stopPropagation: () => void };
  export type KeyboardEvent = { key: string };
  export const createElement: any;
}

declare namespace JSX {
  interface IntrinsicElements {
    [elemName: string]: any;
  }
}
