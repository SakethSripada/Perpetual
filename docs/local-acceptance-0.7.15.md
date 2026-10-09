# Local acceptance - 0.7.15

October 8, 2026. Extension-only reasoning animation; Desktop unchanged.

- A decorative thumb and progress fill share a 180ms ease-out transition. The
  native range retains pointer/keyboard semantics and focus indication.
- Pointer movement during dragging disables interpolation. Reduced-motion CSS
  disables both transitions. Selection and persistence are not delayed.
- 97 tests and extension/webview type checks passed.
- Browser checked Home/End selection at 391x760. After End, the thumb was observed
  at an intermediate 97.33px with fill 108.33px, then settled at 224px with fill
  235px on a 246px rail; both computed transitions were 180ms with identical easing.
