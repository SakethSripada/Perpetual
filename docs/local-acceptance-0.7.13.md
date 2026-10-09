# Local acceptance - 0.7.13

October 8, 2026. Extension-only reasoning default and slider alignment fix.

- Missing or invalid defaults resolve to Medium when supported, otherwise the
  first supported level. Valid reported defaults and saved supported choices
  remain effective; models without adjustable reasoning remain unchanged.
- The rounded rail spans the thumb's full travel area. Tick centres and progress
  endpoints include the thumb radius, removing the protruding first dot.
- 97 tests and both type checks passed, including missing-default, saved-choice,
  unsupported-choice and single-level assertions. VS Code activation: 1 passed.
- Browser fixture verified missing-default Medium, minimum-stop alignment and
  reset to Medium at 391x760. Desktop reference remains unchanged.
