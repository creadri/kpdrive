//! Which plural form a count takes, per the catalog's own rule.
//!
//! Every catalog header carries an expression in a small C-like language, such
//! as Russian's `n%10==1 && n%100!=11 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 ||
//! n%100>=20) ? 1 : 2`. The gettext crate ships an evaluator for these, but it
//! gets nested conditionals wrong: with Arabic's six-way rule it answered "one
//! item" for every count above one, and with Russian's it answered the same
//! form for all of them. So the rule is evaluated here instead, and the crate
//! is told to use this.

/// A parsed plural rule. Cheap to evaluate: the expression is a few dozen
/// nodes and is walked once per translated count.
#[derive(Debug, Clone, PartialEq)]
pub enum Rule {
    N,
    Number(u64),
    Not(Box<Rule>),
    Binary(Op, Box<Rule>, Box<Rule>),
    If(Box<Rule>, Box<Rule>, Box<Rule>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Op {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

impl Rule {
    /// The form index for `n`, or 0 when the rule cannot say.
    pub fn form(&self, n: u64) -> usize {
        self.eval(n) as usize
    }

    fn eval(&self, n: u64) -> u64 {
        match self {
            Rule::N => n,
            Rule::Number(v) => *v,
            Rule::Not(inner) => (inner.eval(n) == 0) as u64,
            Rule::If(cond, yes, no) => {
                if cond.eval(n) != 0 {
                    yes.eval(n)
                } else {
                    no.eval(n)
                }
            }
            Rule::Binary(op, a, b) => {
                let (a, b) = (a.eval(n), b.eval(n));
                match op {
                    Op::Or => (a != 0 || b != 0) as u64,
                    Op::And => (a != 0 && b != 0) as u64,
                    Op::Eq => (a == b) as u64,
                    Op::Ne => (a != b) as u64,
                    Op::Lt => (a < b) as u64,
                    Op::Gt => (a > b) as u64,
                    Op::Le => (a <= b) as u64,
                    Op::Ge => (a >= b) as u64,
                    Op::Add => a.saturating_add(b),
                    Op::Sub => a.saturating_sub(b),
                    Op::Mul => a.saturating_mul(b),
                    // A rule dividing by zero is a broken rule, not a crash.
                    Op::Div => a.checked_div(b).unwrap_or(0),
                    Op::Rem => a.checked_rem(b).unwrap_or(0),
                }
            }
        }
    }
}

/// Parses the `plural=` half of a `Plural-Forms` header. `None` when the
/// expression is not one this understands, in which case the caller should
/// fall back to the English rule rather than guess.
pub fn parse(source: &str) -> Option<Rule> {
    let tokens = tokenize(source)?;
    let mut p = Parser { tokens: &tokens, at: 0 };
    let rule = p.ternary()?;
    // Trailing rubbish means it was not understood, whatever parsed so far.
    match p.at == p.tokens.len() {
        true => Some(rule),
        false => None,
    }
}

#[derive(Debug, PartialEq, Clone)]
enum Token {
    N,
    Number(u64),
    Op(Op),
    Not,
    Question,
    Colon,
    Open,
    Close,
}

fn tokenize(src: &str) -> Option<Vec<Token>> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let two = src.get(i..i + 2);
        match () {
            _ if b.is_ascii_whitespace() => i += 1,
            _ if two == Some("&&") => {
                out.push(Token::Op(Op::And));
                i += 2;
            }
            _ if two == Some("||") => {
                out.push(Token::Op(Op::Or));
                i += 2;
            }
            _ if two == Some("==") => {
                out.push(Token::Op(Op::Eq));
                i += 2;
            }
            _ if two == Some("!=") => {
                out.push(Token::Op(Op::Ne));
                i += 2;
            }
            _ if two == Some("<=") => {
                out.push(Token::Op(Op::Le));
                i += 2;
            }
            _ if two == Some(">=") => {
                out.push(Token::Op(Op::Ge));
                i += 2;
            }
            _ if b == b'n' => {
                out.push(Token::N);
                i += 1;
            }
            _ if b.is_ascii_digit() => {
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                out.push(Token::Number(src[start..i].parse().ok()?));
            }
            _ => {
                out.push(match b {
                    b'?' => Token::Question,
                    b':' => Token::Colon,
                    b'(' => Token::Open,
                    b')' => Token::Close,
                    b'!' => Token::Not,
                    b'<' => Token::Op(Op::Lt),
                    b'>' => Token::Op(Op::Gt),
                    b'+' => Token::Op(Op::Add),
                    b'-' => Token::Op(Op::Sub),
                    b'*' => Token::Op(Op::Mul),
                    b'/' => Token::Op(Op::Div),
                    b'%' => Token::Op(Op::Rem),
                    // A stray `;` ends the expression; anything else is unknown.
                    b';' => return Some(out),
                    _ => return None,
                });
                i += 1;
            }
        }
    }
    Some(out)
}

struct Parser<'a> {
    tokens: &'a [Token],
    at: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn eat(&mut self, want: &Token) -> bool {
        if self.peek() == Some(want) {
            self.at += 1;
            return true;
        }
        false
    }

    /// `a ? b : c`, right-associative, which is the part the crate's own
    /// evaluator gets wrong: every language with more than two forms chains
    /// these, and the tail has to parse as one whole conditional.
    fn ternary(&mut self) -> Option<Rule> {
        let cond = self.binary(0)?;
        if !self.eat(&Token::Question) {
            return Some(cond);
        }
        let yes = self.ternary()?;
        if !self.eat(&Token::Colon) {
            return None;
        }
        let no = self.ternary()?;
        Some(Rule::If(Box::new(cond), Box::new(yes), Box::new(no)))
    }

    /// Precedence climbing, loosest first, the same order C uses.
    fn binary(&mut self, level: usize) -> Option<Rule> {
        const LEVELS: [&[Op]; 6] = [
            &[Op::Or],
            &[Op::And],
            &[Op::Eq, Op::Ne],
            &[Op::Lt, Op::Gt, Op::Le, Op::Ge],
            &[Op::Add, Op::Sub],
            &[Op::Mul, Op::Div, Op::Rem],
        ];
        if level == LEVELS.len() {
            return self.unary();
        }
        let mut left = self.binary(level + 1)?;
        while let Some(Token::Op(op)) = self.peek().cloned() {
            if !LEVELS[level].contains(&op) {
                break;
            }
            self.at += 1;
            let right = self.binary(level + 1)?;
            left = Rule::Binary(op, Box::new(left), Box::new(right));
        }
        Some(left)
    }

    fn unary(&mut self) -> Option<Rule> {
        if self.eat(&Token::Not) {
            return Some(Rule::Not(Box::new(self.unary()?)));
        }
        match self.peek().cloned() {
            Some(Token::N) => {
                self.at += 1;
                Some(Rule::N)
            }
            Some(Token::Number(v)) => {
                self.at += 1;
                Some(Rule::Number(v))
            }
            Some(Token::Open) => {
                self.at += 1;
                let inner = self.ternary()?;
                self.eat(&Token::Close).then_some(inner)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rules gettext ships for these languages, with the answers a speaker
    /// would give. Arabic and the Slavic three are what the crate got wrong.
    #[test]
    fn real_languages_count_correctly() {
        let english = parse("n != 1").expect("parses");
        assert_eq!([0, 1, 2, 5].map(|n| english.form(n)), [1, 0, 1, 1]);

        let french = parse("n > 1").expect("parses");
        assert_eq!([0, 1, 2, 1568].map(|n| french.form(n)), [0, 0, 1, 1]);

        let arabic = parse("n==0 ? 0 : n==1 ? 1 : n==2 ? 2 : n%100>=3 && n%100<=10 ? 3 : n%100>=11 ? 4 : 5").expect("parses");
        // 102 is not a dual: only a literal two takes form 2, and 100 and 102
        // both fall through to the last form.
        assert_eq!([0, 1, 2, 3, 10, 11, 99, 100, 102, 1568].map(|n| arabic.form(n)), [0, 1, 2, 3, 3, 4, 4, 5, 5, 4]);

        let russian = parse("n%10==1 && n%100!=11 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2").expect("parses");
        assert_eq!([1, 2, 5, 11, 21, 22, 25, 1568].map(|n| russian.form(n)), [0, 1, 2, 2, 0, 1, 2, 2]);

        let polish = parse("n==1 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2").expect("parses");
        assert_eq!([1, 2, 5, 12, 22, 25].map(|n| polish.form(n)), [0, 1, 2, 2, 1, 2]);

        let czech = parse("(n==1) ? 0 : (n>=2 && n<=4) ? 1 : 2").expect("parses");
        assert_eq!([1, 2, 4, 5].map(|n| czech.form(n)), [0, 1, 1, 2]);

        let romanian = parse("n==1 ? 0 : (n==0 || (n%100 > 0 && n%100 < 20)) ? 1 : 2").expect("parses");
        assert_eq!([0, 1, 2, 19, 20, 101].map(|n| romanian.form(n)), [1, 0, 1, 1, 2, 1]);

        let one_form = parse("0").expect("parses");
        assert_eq!([0, 1, 7].map(|n| one_form.form(n)), [0, 0, 0]);
    }

    #[test]
    fn nonsense_is_refused_rather_than_guessed() {
        assert!(parse("nplurals=2").is_none(), "a whole header is not an expression");
        assert!(parse("n ? 1").is_none(), "an unfinished conditional");
        assert!(parse("n @ 2").is_none());
        assert!(parse("").is_none());
        // A trailing semicolon is how the header writes it, and is fine.
        assert!(parse("n != 1;").is_some());
        // Division by zero answers rather than panics.
        assert_eq!(parse("n / 0").expect("parses").form(5), 0);
    }
}
