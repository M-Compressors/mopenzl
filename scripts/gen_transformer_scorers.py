#!/usr/bin/env python3
"""Mechanically converts the Transformer's generated C scorers to Rust.

Input:  openzl-ffi/vendor/openzl/src/openzl/compress/selectors/transformer/
        generated/score_numeric{8,16,32,64}.c (and numeric_features*.h for
        the feature field types)
Output: src/compressor/core/graphs/transformer/scorers/num{8,16,32,64}.rs

Nothing numeric is typed by hand. The script

* parses the weight, bias and normalization tables and re-emits each float
  with the shortest decimal that rounds to the same `f32` as the C literal
  (so clippy's `excessive_precision` stays quiet and the value is identical);
* parses the feature-extraction expressions (`input[i] = ...;`) with a small
  C expression parser and prints them as Rust with the same evaluation
  order, the same intermediate types and the same casts (no fused
  multiply-adds are ever introduced);
* checks that the two dense layers, ReLU and the sigmoid are textually the
  ones the hand-written `scorers/mod.rs` implements, and fails otherwise, so
  a regenerated model with a different network shape is noticed.

Usage (from the repository root, then `cargo fmt`):

    python3 scripts/gen_transformer_scorers.py

Run it again, and commit the result, whenever the vendored scorers change.
"""

import re
import struct
import subprocess
import sys
from fractions import Fraction
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
C_DIR = (
    ROOT
    / "openzl-ffi/vendor/openzl/src/openzl/compress/selectors/transformer"
)
OUT_DIR = ROOT / "src/compressor/core/graphs/transformer/scorers"

C_TO_RUST_TYPE = {
    "uint64_t": "u64",
    "int64_t": "i64",
    "size_t": "usize",
    "double": "f64",
}

OP_NAMES = {
    "INVALID": None,
    "CONSTANT": "Constant",
    "BITPACK": "Bitpack",
    "DELTA_INT": "DeltaInt",
    "DIVIDE_BY_GCD": "DivideByGcd",
    "ENTROPY": "Entropy",
    "FIELD_LZ": "FieldLz",
    "FSE": "Fse",
    "RANGE_PACK": "RangePack",
    "SPARSE_NUM": "SparseNum",
    "STORE": "Store",
    "TOKENIZE_NUMERIC": "TokenizeNumeric",
    "TOKENIZE_NUMERIC_SORTED": "TokenizeNumericSorted",
    "ZSTD": "Zstd",
}


# ---------------------------------------------------------------- floats --


def f32_bits_round_nearest_even(text):
    """Bits of the `f32` nearest to the decimal `text` (correct rounding)."""
    exact = Fraction(text)
    approx = struct.unpack("<I", struct.pack("<f", float(exact)))[0]
    best = None
    for bits in (approx - 1, approx, approx + 1):
        if bits < 0:
            continue
        value = Fraction(struct.unpack("<f", struct.pack("<I", bits))[0])
        error = abs(value - exact)
        key = (error, bits & 1)
        if best is None or key < best[0]:
            best = (key, bits)
    return best[1]


def f32_from_bits(bits):
    return struct.unpack("<f", struct.pack("<I", bits))[0]


def shortest_f32_literal(text):
    """Shortest decimal literal that denotes the same f32 as `text`."""
    bits = f32_bits_round_nearest_even(text)
    value = f32_from_bits(bits)
    for digits in range(1, 10):
        candidate = "%.*e" % (digits - 1, value)
        if f32_bits_round_nearest_even(candidate) == bits:
            break
    mantissa, exponent = candidate.split("e")
    exponent = int(exponent)
    negative = mantissa.startswith("-")
    digit_string = mantissa.lstrip("-").replace(".", "")
    digit_string = digit_string.rstrip("0") or "0"
    if digit_string == "0":
        out = "0.0"
    elif -5 <= exponent < 9:
        if exponent >= 0:
            integer = digit_string[: exponent + 1].ljust(exponent + 1, "0")
            fraction = digit_string[exponent + 1 :] or "0"
            out = integer + "." + fraction
        else:
            out = "0." + "0" * (-exponent - 1) + digit_string
    else:
        out = digit_string[0] + ("." + digit_string[1:] if len(digit_string) > 1 else "") + "e%d" % exponent
    if negative:
        out = "-" + out
    assert f32_bits_round_nearest_even(out) == bits, (text, out)
    return out + "_f32", bits


# ----------------------------------------------------- C expression parser --

TOKEN = re.compile(
    r"""\s*(?:
    (?P<num>\d+\.\d*(?:[eE][-+]?\d+)?|\.\d+(?:[eE][-+]?\d+)?|\d+[eE][-+]?\d+|\d+)(?P<suffix>[uUlLfF]*)
    |(?P<id>[A-Za-z_][A-Za-z_0-9]*)
    |(?P<op>->|&&|\|\||==|!=|<=|>=|[-+*/<>?:()\[\],!])
    )""",
    re.X,
)


def tokenize(text):
    tokens = []
    position = 0
    text = text.rstrip()
    while position < len(text):
        match = TOKEN.match(text, position)
        if not match or match.end() == position:
            raise SystemExit("cannot tokenize at: " + text[position : position + 40])
        position = match.end()
        if match.group("num") is not None:
            tokens.append(("num", match.group("num"), match.group("suffix")))
        elif match.group("id") is not None:
            tokens.append(("id", match.group("id"), ""))
        else:
            tokens.append(("op", match.group("op"), ""))
    return tokens


class Expr:
    """A typed Rust expression with the precedence of its top operator."""

    # precedence: higher binds tighter. 100 = atom.
    def __init__(self, text, ty, prec=100, is_cast=False):
        self.text, self.ty, self.prec, self.is_cast = text, ty, prec, is_cast

    def wrapped(self, min_prec):
        return "(" + self.text + ")" if self.prec < min_prec else self.text


PREC = {"||": 1, "&&": 2, "==": 3, "!=": 3, "<": 4, ">": 4, "<=": 4, ">=": 4,
        "+": 5, "-": 5, "*": 6, "/": 6}
CAST_TYPES = {"float": "f32", "double": "f64", "long long": "i64"}


class Parser:
    def __init__(self, tokens, field_types, table_names):
        self.t = tokens
        self.i = 0
        self.fields = field_types
        self.tables = table_names

    def peek(self, k=0):
        return self.t[self.i + k] if self.i + k < len(self.t) else ("eof", "", "")

    def take(self, value=None):
        token = self.peek()
        if value is not None and token[1] != value:
            raise SystemExit("expected %r, got %r" % (value, token))
        self.i += 1
        return token

    def parse(self):
        expr = self.ternary()
        if self.peek()[0] != "eof":
            raise SystemExit("trailing tokens: %r" % (self.t[self.i :],))
        return expr

    def ternary(self):
        cond = self.binary(1)
        if self.peek()[1] == "?":
            self.take("?")
            a = self.ternary()
            self.take(":")
            b = self.ternary()
            a, b = unify(a, b)
            if b.prec == 0 and b.text.startswith("if "):
                return Expr("if %s { %s } else %s" % (cond.text, a.text, b.text), a.ty, 0)
            return Expr("if %s { %s } else { %s }" % (cond.text, a.text, b.text), a.ty, 0)
        return cond

    def binary(self, min_prec):
        left = self.unary()
        while self.peek()[0] == "op" and self.peek()[1] in PREC and PREC[self.peek()[1]] >= min_prec:
            op = self.take()[1]
            prec = PREC[op]
            right = self.binary(prec + 1)
            left = make_binary(op, prec, left, right)
        return left

    def unary(self):
        token = self.peek()
        if token[1] == "-":
            self.take()
            operand = self.unary()
            return Expr("-" + operand.wrapped(100), operand.ty, 90)
        if token[1] == "(":
            cast = self.try_cast()
            if cast is not None:
                return cast
        return self.postfix()

    def try_cast(self):
        # "(" type ")" unary
        save = self.i
        self.take("(")
        names = []
        while self.peek()[0] == "id":
            names.append(self.take()[1])
        name = " ".join(names)
        if name in CAST_TYPES and self.peek()[1] == ")":
            self.take(")")
            return cast(CAST_TYPES[name], self.unary())
        self.i = save
        return None

    def postfix(self):
        token = self.take()
        if token[0] == "num":
            return number(token)
        if token[1] == "(":
            inner = self.ternary()
            self.take(")")
            return inner
        if token[0] == "id":
            name = token[1]
            if self.peek()[1] == "(":
                self.take("(")
                arg = self.ternary()
                self.take(")")
                return call(name, arg)
            if self.peek()[1] == "->":
                self.take("->")
                field = self.take()[1]
                if field not in self.fields:
                    raise SystemExit("unknown feature field " + field)
                return Expr("f." + field, self.fields[field])
            if self.peek()[1] == "[":
                self.take("[")
                index = self.take()[1]
                self.take("]")
                if name not in self.tables:
                    raise SystemExit("unknown table " + name)
                return Expr("%s[%s]" % (self.tables[name], index), "f32")
        raise SystemExit("unexpected token %r" % (token,))


def number(token):
    _, digits, suffix = token
    suffix = suffix.lower()
    if re.fullmatch(r"\d+", digits) and suffix in ("", "ull", "u", "ul", "l", "ll"):
        if suffix == "":
            return Expr(digits, "int")
        return Expr(digits + "u64", "u64")
    if suffix in ("", "l"):
        text = digits
        return Expr(text if ("." in text or "e" in text.lower()) else text + ".0", "f64")
    raise SystemExit("unsupported literal %r" % (token,))


def call(name, arg):
    functions = {"log": "math::ln", "log2": "math::log2"}
    if name not in functions or arg.ty != "f64":
        raise SystemExit("unsupported call %s(%s)" % (name, arg.ty))
    return Expr("%s(%s)" % (functions[name], arg.text), "f64")


def cast(target, operand):
    if operand.ty == target:
        return operand
    if operand.ty == "bool":
        one, zero = ("1.0", "0.0") if target in ("f32", "f64") else ("1", "0")
        return Expr("if %s { %s } else { %s }" % (operand.text, one, zero), target, 0)
    if operand.ty == "int" and target in ("f32", "f64"):
        # An integer literal converted to floating point is the float literal.
        return Expr(operand.text + ".0", target)
    if operand.ty == "u64" and operand.text.endswith("u64") and target == "f64":
        return literal_u64_to_f64(operand)
    return Expr("%s as %s" % (operand.wrapped(8), target), target, 7, True)


def literal_u64_to_f64(expr):
    """A `...ULL` literal converted to double, exactly when representable."""
    value = int(expr.text[:-3])
    if float(value) != value:
        raise SystemExit("u64 literal %d is not exact in f64" % value)
    return Expr("%d.0" % value, "f64")


def unify(a, b):
    if a.ty == b.ty:
        return a, b
    if a.ty == "f64" and b.ty == "u64" and b.text.endswith("u64"):
        return a, literal_u64_to_f64(b)
    if b.ty == "f64" and a.ty == "u64" and a.text.endswith("u64"):
        return literal_u64_to_f64(a), b
    if a.ty == "int" and b.ty in ("f64", "f32"):
        return Expr(a.text + ".0", b.ty), b
    if b.ty == "int" and a.ty in ("f64", "f32"):
        return a, Expr(b.text + ".0", a.ty)
    if a.ty == "int":
        return Expr(a.text, b.ty), b
    if b.ty == "int":
        return a, Expr(b.text, a.ty)
    raise SystemExit("type mismatch %s vs %s (%s | %s)" % (a.ty, b.ty, a.text, b.text))


def make_binary(op, prec, left, right):
    left, right = unify(left, right)
    ty = "bool" if op in ("&&", "||", "==", "!=", "<", ">", "<=", ">=") else left.ty
    # Left-associative: the right operand needs one more level to avoid a
    # reassociation; comparisons chain only through parentheses.
    lt = left.wrapped(prec)
    rt = right.wrapped(prec + 1)
    if op in ("<", "<=") and left.is_cast:
        lt = "(" + left.text + ")"
    if prec in (3, 4) and left.prec in (3, 4):
        lt = "(" + left.text + ")"
    return Expr("%s %s %s" % (lt, op, rt), ty, prec)


# --------------------------------------------------------------- C parsing --


def parse_struct_fields(path, struct_name):
    text = path.read_text()
    body = re.search(r"typedef struct \{(.*?)\}\s*" + struct_name + ";", text, re.S).group(1)
    body = re.sub(r"/\*.*?\*/", "", body, flags=re.S)
    fields = {}
    for ctype, name in re.findall(r"\b(uint64_t|int64_t|size_t|double)\s+(\w+)\s*;", body):
        fields[name] = C_TO_RUST_TYPE[ctype]
    return fields


def extract_braced(text, start):
    depth = 0
    for index in range(start, len(text)):
        if text[index] == "{":
            depth += 1
        elif text[index] == "}":
            depth -= 1
            if depth == 0:
                return text[start : index + 1]
    raise SystemExit("unbalanced braces")


def parse_table(text, name, defines):
    match = re.search(r"static const float " + name + r"((?:\[[^\]]+\])+)\s*=\s*", text)
    if not match:
        raise SystemExit("missing table " + name)
    dims = [
        int(defines.get(d, d)) for d in re.findall(r"\[([^\]]+)\]", match.group(1))
    ]
    body = extract_braced(text, match.end())
    values = re.findall(r"[-+]?\d+\.\d*(?:[eE][-+]?\d+)?f", body)
    expected = 1
    for dim in dims:
        expected *= dim
    if len(values) != expected:
        raise SystemExit("%s: %d values, expected %d" % (name, len(values), expected))
    return dims, [v[:-1] for v in values]


APPROX_CONSTANTS = [
    3.14159265358979, 1.57079632679490, 0.78539816339745, 0.31830988618379,
    0.63661977236758, 1.12837916709551, 0.69314718055994, 2.30258509299404,
    0.43429448190325, 1.44269504088896, 0.30102999566398, 2.71828182845904,
    1.41421356237309, 0.70710678118654, 6.28318530717958, 1.04719755119660,
    0.52359877559829, 3.32192809488736,
]


def literal(text):
    lit, bits = shortest_f32_literal(text)
    value = f32_from_bits(bits)
    for const in APPROX_CONSTANTS:
        # clippy::approx_constant (deny): needs >= ~4 matching digits.
        if abs(abs(value) - const) < 5e-4 * const and len(lit) > 6:
            digits = re.sub(r"[^0-9]", "", lit.split("e")[0])
            if len(digits) >= 4 and str(const).replace(".", "")[: len(digits.lstrip("0"))] == digits.lstrip("0")[: len(str(const).replace(".", ""))]:
                return "f32::from_bits(0x%08X)" % bits
    return lit


def emit_array(values, dims):
    if len(dims) == 1:
        return "[" + ", ".join(literal(v) for v in values) + "]"
    step = len(values) // dims[0]
    return "[" + ", ".join(emit_array(values[i * step : (i + 1) * step], dims[1:]) for i in range(dims[0])) + "]"


def rust_type(dims):
    t = "f32"
    for dim in reversed(dims):
        t = "[%s; %d]" % (t, dim)
    return t


NETWORK_TEMPLATE = {
    "relu": "return x > 0.0f ? x : 0.0f;",
    "sigmoid": (
        "if (x > 15.0f) return 1.0f; if (x < -15.0f) return 0.0f; "
        "return 1.0f / (1.0f + expf(-x));"
    ),
}


def squash(text):
    return re.sub(r"\s+", " ", text).strip()


def check_network(text, prefix, n_hidden, n_features, n_outputs):
    """Fails unless the C network is the one `scorers/mod.rs` implements."""
    for name, body in NETWORK_TEMPLATE.items():
        match = re.search(
            r"static float %s%s\(float x\)\s*\{(.*?)\n\}" % (prefix, name), text, re.S
        )
        if not match or squash(match.group(1)).replace("{ ", "").replace(" }", "") != squash(body).replace("{ ", "").replace(" }", ""):
            # Normalise braces around single statements.
            got = squash(match.group(1)) if match else "<missing>"
            norm = lambda s: re.sub(r"[{}]", "", s).replace("  ", " ").strip()
            if norm(got) != norm(body):
                raise SystemExit("%s%s differs from the template: %s" % (prefix, name, got))
    layers = squash(text[text.index("/* Layer 1 */") : text.index("/* Copy scores")])
    expected = (
        "/* Layer 1 */ for (int j = 0; j < H; j++) { float sum = PREFIX_b1[j]; "
        "for (int i = 0; i < I; i++) { sum += input[i] * PREFIX_w1[i][j]; } "
        "hidden[j] = PREFIX_relu(sum); } /* Layer 2 */ for (int j = 0; j < O; j++) "
        "{ float sum = PREFIX_b2[j]; for (int i = 0; i < H; i++) "
        "{ sum += hidden[i] * PREFIX_w2[i][j]; } if (logits_out) logits_out[j] = sum; "
        "output[j] = PREFIX_sigmoid(sum); }"
    )
    expected = (
        expected.replace("PREFIX", prefix.rstrip("_"))
        .replace("H;", "%d;" % n_hidden, 2)
        .replace("I;", "%d;" % n_features)
        .replace("O;", "%d;" % n_outputs)
    )
    expected = expected.replace("j < H;", "j < %d;" % n_hidden)
    if layers != expected:
        raise SystemExit("network of %s differs from the template:\n%s\n%s" % (prefix, layers, expected))


def generate(width):
    source = (C_DIR / ("generated/score_numeric%d.c" % width)).read_text()
    prefix = "TRS%d_" % width
    defines = dict(re.findall(r"#define (TRS%d_N_\w+) (\d+)" % width, source))
    n_features = int(defines["TRS%d_N_FEATURES" % width])
    n_outputs = int(defines["TRS%d_N_OUTPUTS" % width])

    ids = re.search(r"operation_ids\[\] = \{(.*?)\};", source, re.S).group(1)
    ops = [OP_NAMES[o] for o in re.findall(r"TRS_GENERIC_NUMERIC_OP_(\w+)", ids)]
    assert len(ops) == n_outputs

    tables = {}
    for name in ("norm_mean", "norm_std", "w1", "b1", "w2", "b2"):
        tables[name] = parse_table(source, prefix + name, defines)
    n_hidden = tables["b1"][0][0]
    assert tables["w1"][0] == [n_features, n_hidden]
    assert tables["w2"][0] == [n_hidden, n_outputs]
    check_network(source, prefix, n_hidden, n_features, n_outputs)

    signature = re.search(
        r"TRS_score_num%d_operations_with_logits\(\s*const (TRS_NumericFeatures\w*)\* f," % width,
        source,
    ).group(1)
    v2 = signature == "TRS_NumericFeaturesV2"
    header = "numeric_features_v2.h" if v2 else "numeric_features.h"
    fields = parse_struct_fields(C_DIR / header, signature)

    body = source[source.index("/* Extract and normalize features */") : source.index("/* Layer 1 */")]
    table_names = {
        prefix + "norm_mean": "NORM_MEAN",
        prefix + "norm_std": "NORM_STD",
    }
    expressions = []
    for index, text in re.findall(r"input\[(\d+)\]\s*=\s*(.*?);\s*(?=input\[|\Z)", body, re.S):
        assert int(index) == len(expressions)
        text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)
        expr = Parser(tokenize(text), fields, table_names).parse()
        assert expr.ty == "f32", (index, expr.ty)
        expressions.append(expr.text)
    assert len(expressions) == n_features

    used = sorted({m for e in expressions for m in re.findall(r"\bf\.(\w+)", e)})
    out = []
    out.append("//! The num%d scorer: `generated/score_numeric%d.c` converted by" % (width, width))
    out.append("//! `scripts/gen_transformer_scorers.py`. Do not edit by hand.")
    out.append("//!")
    out.append("//! Features read: `%s`." % "`, `".join(used))
    out.append("")
    out.append("use super::{Features, Op, math};" if not v2 else "use super::{FeaturesV2 as Features, Op, math};")
    out.append("")
    out.append("/// Number of input features.")
    out.append("pub(crate) const N_FEATURES: usize = %d;" % n_features)
    out.append("/// Number of scored operations.")
    out.append("pub(crate) const N_OUTPUTS: usize = %d;" % n_outputs)
    out.append("")
    out.append("/// Operation scored by each output neuron.")
    out.append("pub(crate) const OPERATION_IDS: [Op; N_OUTPUTS] = [%s];" % ", ".join("Op::" + o for o in ops))
    out.append("")
    docs = {"norm_mean": "Feature means.", "norm_std": "Feature standard deviations.",
            "w1": "First layer weights, indexed `[feature][hidden]`.", "b1": "First layer biases.",
            "w2": "Second layer weights, indexed `[hidden][output]`.", "b2": "Second layer biases."}
    for name in ("norm_mean", "norm_std", "w1", "b1", "w2", "b2"):
        dims, values = tables[name]
        out.append("/// " + docs[name])
        out.append("pub(crate) static %s: %s = %s;" % (name.upper(), rust_type(dims), emit_array(values, dims)))
        out.append("")
    out.append("/// The normalized network inputs (`input[]` of the C scorer).")
    out.append("pub(crate) fn inputs(f: &Features) -> [f32; N_FEATURES] {")
    out.append("    [")
    for expression in expressions:
        out.append("        (%s)," % expression if False else "        %s," % expression)
    out.append("    ]")
    out.append("}")
    out.append("")
    return "\n".join(out)


def main():
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    for width in (8, 16, 32, 64):
        path = OUT_DIR / ("num%d.rs" % width)
        path.write_text(generate(width))
        print("wrote", path.relative_to(ROOT))
    try:
        subprocess.run(["rustfmt", "--edition", "2024"] + [str(OUT_DIR / ("num%d.rs" % w)) for w in (8, 16, 32, 64)], check=True)
    except (OSError, subprocess.CalledProcessError) as error:
        print("rustfmt not run:", error, file=sys.stderr)


if __name__ == "__main__":
    main()
