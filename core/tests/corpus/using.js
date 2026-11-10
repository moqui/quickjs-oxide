// Explicit resource management (FORK-EXPLICIT-RESOURCE-MANAGEMENT-001):
// `using`, `await using`, DisposableStack, AsyncDisposableStack and
// SuppressedError.
var log = [];
function resource(name, fail) {
  return {
    [Symbol.dispose]() {
      log.push(name);
      if (fail) throw new Error(name);
    },
  };
}
function flush(label) {
  print(label + ": " + log.join(" "));
  log = [];
}

// Reverse order at the end of a block; null and undefined are skipped.
{
  using a = resource("a"), b = resource("b");
  using c = null, d = undefined;
  log.push("body");
}
flush("block");

// Every abrupt exit disposes.
function returns() {
  using r = resource("r");
  return "returned";
}
print(returns());
flush("return");
for (var i = 0; i < 3; i++) {
  using r = resource("i" + i);
  if (i == 0) continue;
  if (i == 1) break;
}
flush("break/continue");
outer: {
  using r = resource("labelled");
  break outer;
}
flush("labelled break");

// A throw from the body and from disposals.
try {
  using a = resource("a", true);
  using b = resource("b", true);
  throw new Error("body");
} catch (e) {
  print(e instanceof SuppressedError, e.error.message, e.suppressed.error.message,
        e.suppressed.suppressed.message);
}
flush("throws");
try {
  using a = resource("only", true);
} catch (e) {
  print(e.message);
}
flush("one throw");

// Initializers that fail.
try {
  using a = resource("first"), b = 1;
} catch (e) {
  print(e.name);
}
flush("not an object");
try {
  using a = {};
} catch (e) {
  print(e.name);
}

// for-of disposes each iteration, before the iterator closes.
var iterable = {
  [Symbol.iterator]() {
    var n = 0;
    return {
      next() { n++; return { value: resource("x" + n), done: n > 3 }; },
      return() { log.push("close"); return {}; },
    };
  },
};
for (using x of iterable) {
  log.push("it");
  if (log.length >= 3) break;
}
flush("for-of");
for (using x = resource("head"); log.length < 2; ) log.push("loop");
flush("for head");

// The bindings are constant.
try {
  using fixed = null;
  fixed = 1;
} catch (e) {
  print(e.name);
}

// A generator's return disposes.
function* generator() {
  using g = resource("generator");
  yield 1;
  yield 2;
}
var it = generator();
it.next();
it.return();
flush("generator");

// %IteratorPrototype%[@@dispose] calls return.
var helper = [1, 2].values().map(function (v) { return v; });
helper[Symbol.dispose]();
print(JSON.stringify(helper.next()));

// SuppressedError.
var suppressed = new SuppressedError(1, 2, "message");
print(suppressed.name, suppressed.error, suppressed.suppressed, suppressed.message,
      SuppressedError.length, Object.prototype.toString.call(suppressed));

// DisposableStack.
var stack = new DisposableStack();
stack.use(resource("used"));
stack.defer(function () { log.push("deferred"); });
print(stack.adopt("value", function (v) { log.push("adopted " + v); }));
var moved = stack.move();
print(stack.disposed, moved.disposed);
moved[Symbol.dispose]();
print(moved.disposed);
flush("stack");
try {
  stack.use(null);
} catch (e) {
  print(e.name);
}
print(Object.prototype.toString.call(moved), DisposableStack.prototype[Symbol.dispose] ===
      DisposableStack.prototype.dispose);

// Syntax.
[
  "using x = null;",
  "{ using x; }",
  "{ using [x] = null; }",
  "if (1) using x = null;",
  "switch (0) { case 0: using x = null; }",
  "for (using x in {}) {}",
  "{ using let = null; }",
].forEach(function (source) {
  try {
    eval(source);
    print("parsed:", source);
  } catch (e) {
    print(e.name + ":", source);
  }
});
var using = [7], of = [[9]];
for (using of of [0]) print("identifier using:", using);
{ using
  x = 1; }
print("two statements:", x);

// await using.
async function asynchronous() {
  {
    await using a = {
      async [Symbol.asyncDispose]() { log.push("async a"); },
    };
    await using n = null;
    using s = resource("sync");
    await using f = resource("sync fallback");
    log.push("body");
  }
  flush("await using");
  for (await using x of [resource("each 1"), resource("each 2")]) log.push("it");
  flush("for await using");
  try {
    await using r = {
      [Symbol.asyncDispose]() { return Promise.reject(new Error("rejected")); },
    };
  } catch (e) {
    print(e.message);
  }
  var async_stack = new AsyncDisposableStack();
  async_stack.defer(async function () { log.push("deferred"); });
  async_stack.use(resource("used"));
  async_stack.adopt(1, function (v) { log.push("adopted " + v); });
  await async_stack.disposeAsync();
  print(async_stack.disposed);
  flush("async stack");
  var failing = new AsyncDisposableStack();
  failing.defer(function () { throw new Error("one"); });
  failing.defer(function () { throw new Error("two"); });
  try {
    await failing[Symbol.asyncDispose]();
  } catch (e) {
    print(e.name, e.error.message, e.suppressed.message);
  }
}
asynchronous().then(function () { print("done"); }, function (e) { print("failed", e); });
