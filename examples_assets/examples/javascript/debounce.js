// Small utilities: closures, higher-order functions and promises.

/**
 * Returns a function that waits `delayMs` after its last call before
 * running `fn`, so a burst of calls runs it once.
 */
function debounce(fn, delayMs) {
  let timer = null;
  return function (...args) {
    clearTimeout(timer);
    timer = setTimeout(() => fn.apply(this, args), delayMs);
  };
}

/** Resolves after `ms` milliseconds. */
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** Retries an async function up to `attempts` times, waiting between tries. */
async function retry(task, attempts = 3, delayMs = 100) {
  let lastError;
  for (let attempt = 1; attempt <= attempts; attempt++) {
    try {
      return await task(attempt);
    } catch (error) {
      lastError = error;
      await sleep(delayMs * attempt);
    }
  }
  throw lastError;
}

/** Groups items by the key `keyOf` returns. */
function groupBy(items, keyOf) {
  const groups = new Map();
  for (const item of items) {
    const key = keyOf(item);
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(item);
  }
  return groups;
}

module.exports = { debounce, sleep, retry, groupBy };
