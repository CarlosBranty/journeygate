// Minimal, zero-dependency JSON Schema (draft 2020-12) validator.
// It supports only the keywords used by the schemas in this folder, so the
// contract tests run with plain `node --test` and no install step.
// Supported: $ref (local "#/$defs/..."), type, const, enum, required,
// properties, patternProperties, additionalProperties, propertyNames,
// minProperties, maxProperties, items, minItems, maxItems, uniqueItems,
// minLength, maxLength, pattern, minimum, maximum, exclusiveMinimum, oneOf. Annotations (title, description, default,
// examples, format, $comment) are ignored.

const typeOf = (v) => {
  if (v === null) return 'null';
  if (Array.isArray(v)) return 'array';
  if (typeof v === 'number') return Number.isInteger(v) ? 'integer' : 'number';
  return typeof v;
};

const matchesType = (value, expected) => {
  const actual = typeOf(value);
  return actual === expected || (expected === 'number' && actual === 'integer');
};

const resolveRef = (root, ref) => {
  if (!ref.startsWith('#/')) throw new Error(`unsupported $ref: ${ref}`);
  return ref
    .slice(2)
    .split('/')
    .reduce((node, key) => {
      if (node == null || !(key in node)) throw new Error(`unresolvable $ref: ${ref}`);
      return node[key];
    }, root);
};

function check(root, schema, value, path, errors) {
  if (schema.$ref) {
    check(root, resolveRef(root, schema.$ref), value, path, errors);
  }
  const at = path || '<root>';

  if (schema.oneOf) {
    const passing = schema.oneOf.filter((sub) => {
      const subErrors = [];
      check(root, sub, value, path, subErrors);
      return subErrors.length === 0;
    }).length;
    if (passing !== 1) errors.push(`${at}: must match exactly one oneOf branch, matched ${passing}`);
  }

  if (schema.type) {
    const types = Array.isArray(schema.type) ? schema.type : [schema.type];
    if (!types.some((t) => matchesType(value, t))) {
      errors.push(`${at}: type must be ${types.join('|')}, got ${typeOf(value)}`);
      return;
    }
  }
  if ('const' in schema && JSON.stringify(schema.const) !== JSON.stringify(value)) {
    errors.push(`${at}: must equal const ${JSON.stringify(schema.const)}`);
  }
  if (schema.enum && !schema.enum.some((e) => JSON.stringify(e) === JSON.stringify(value))) {
    errors.push(`${at}: must be one of enum ${JSON.stringify(schema.enum)}`);
  }

  if (typeof value === 'string') {
    if (schema.minLength !== undefined && value.length < schema.minLength) {
      errors.push(`${at}: minLength ${schema.minLength}`);
    }
    if (schema.maxLength !== undefined && value.length > schema.maxLength) {
      errors.push(`${at}: maxLength ${schema.maxLength}`);
    }
    if (schema.pattern && !new RegExp(schema.pattern, 'u').test(value)) {
      errors.push(`${at}: does not match pattern ${schema.pattern}`);
    }
  }

  if (typeof value === 'number') {
    if (schema.minimum !== undefined && value < schema.minimum) errors.push(`${at}: minimum ${schema.minimum}`);
    if (schema.maximum !== undefined && value > schema.maximum) errors.push(`${at}: maximum ${schema.maximum}`);
    if (schema.exclusiveMinimum !== undefined && value <= schema.exclusiveMinimum) {
      errors.push(`${at}: exclusiveMinimum ${schema.exclusiveMinimum}`);
    }
  }

  if (Array.isArray(value)) {
    if (schema.minItems !== undefined && value.length < schema.minItems) {
      errors.push(`${at}: minItems ${schema.minItems}`);
    }
    if (schema.maxItems !== undefined && value.length > schema.maxItems) {
      errors.push(`${at}: maxItems ${schema.maxItems}`);
    }
    if (schema.uniqueItems) {
      const seen = new Set(value.map((v) => JSON.stringify(v)));
      if (seen.size !== value.length) errors.push(`${at}: items must be unique`);
    }
    if (schema.items) {
      value.forEach((item, i) => check(root, schema.items, item, `${path}[${i}]`, errors));
    }
  }

  if (typeOf(value) === 'object') {
    const keys = Object.keys(value);
    for (const req of schema.required ?? []) {
      if (!(req in value)) errors.push(`${at}: missing required property "${req}"`);
    }
    if (schema.minProperties !== undefined && keys.length < schema.minProperties) {
      errors.push(`${at}: minProperties ${schema.minProperties}`);
    }
    if (schema.maxProperties !== undefined && keys.length > schema.maxProperties) {
      errors.push(`${at}: maxProperties ${schema.maxProperties}`);
    }
    const props = schema.properties ?? {};
    const patterns = Object.entries(schema.patternProperties ?? {}).map(([p, s]) => [new RegExp(p, 'u'), s]);
    for (const key of keys) {
      const child = path ? `${path}.${key}` : key;
      if (schema.propertyNames) {
        const nameErrors = [];
        check(root, schema.propertyNames, key, child, nameErrors);
        if (nameErrors.length) errors.push(`${child}: invalid property name (${nameErrors.join('; ')})`);
      }
      let matched = false;
      if (key in props) {
        matched = true;
        check(root, props[key], value[key], child, errors);
      }
      for (const [re, s] of patterns) {
        if (re.test(key)) {
          matched = true;
          check(root, s, value[key], child, errors);
        }
      }
      if (!matched && schema.additionalProperties !== undefined) {
        if (schema.additionalProperties === false) {
          errors.push(`${child}: additional property not allowed`);
        } else if (typeof schema.additionalProperties === 'object') {
          check(root, schema.additionalProperties, value[key], child, errors);
        }
      }
    }
  }
}

/** Returns a list of human-readable errors; an empty list means valid. */
export function validate(schema, value) {
  const errors = [];
  check(schema, schema, value, '', errors);
  return errors;
}
