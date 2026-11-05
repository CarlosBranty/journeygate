// Patterns shared by the test API (docs/contracts/demo-app.md, "Data model").

export const NAME_PATTERN = /^[A-Za-z0-9._-]{1,200}$/;
export const ROLE_PATTERN = /^[a-z][a-z0-9_-]*$/;

export const isNonNegativeInteger = (v) => Number.isInteger(v) && v >= 0;
export const isPlainObject = (v) => typeof v === 'object' && v !== null && !Array.isArray(v);
