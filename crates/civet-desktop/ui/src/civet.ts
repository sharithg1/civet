// docs/INTERFACES.md §6 names `ui/src/civet.ts` as the single module wrapping
// invoke / Channel / listen. The implementation lives in backend.ts (typed
// client + mock selection); this module is the contract-named entry point.
export * from './backend'
export * from './types'
