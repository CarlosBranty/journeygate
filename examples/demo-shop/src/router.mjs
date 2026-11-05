// Minimal method + path router with `:name` segments. First match wins.

const split = (path) => path.split('/').slice(1);

export function createRouter() {
  const routes = [];

  const add = (method, pattern, handler) => {
    routes.push({ method, segments: split(pattern), handler });
  };

  const match = (method, path) => {
    const parts = split(path);
    for (const route of routes) {
      if (route.method !== method || route.segments.length !== parts.length) continue;
      const params = matchSegments(route.segments, parts);
      if (params) return { handler: route.handler, params };
    }
    return null;
  };

  return { add, match };
}

function matchSegments(segments, parts) {
  const params = {};
  for (let i = 0; i < segments.length; i++) {
    const seg = segments[i];
    if (seg.startsWith(':')) {
      if (parts[i] === '') return null;
      try {
        params[seg.slice(1)] = decodeURIComponent(parts[i]);
      } catch {
        return null;
      }
    } else if (seg !== parts[i]) {
      return null;
    }
  }
  return params;
}
