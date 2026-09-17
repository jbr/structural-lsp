//! Small, dependency-free semantic navigation and assist fixture.

// @decl Config | Config
#[derive(Debug, Clone)]
pub struct Config {
    // @decl limit | limit
    pub limit: usize,
}

// @decl Mode | Mode
pub enum Mode {
    Fast,
    Safe,
}

// @decl Label | Label
pub trait Label {
    fn label(&self) -> String;
}

impl Config {
    // @decl new | new
    pub fn new(limit: usize) -> Self {
        Self { limit }
    }

    // @decl doubled | doubled
    pub fn doubled(&self) -> usize {
        // @expr limit | self.limit * 2
        self.limit * 2
    }
}

impl Label for Config {
    // @decl label | label
    fn label(&self) -> String {
        // @expr format | format!("limit={}", self.limit)
        format!("limit={}", self.limit)
    }
}

// @decl choose | choose
pub fn choose(mode: Mode) -> usize {
    // @expr Mode | match mode { Mode::Fast => 1, Mode::Safe => 2 }
    match mode { Mode::Fast => 1, Mode::Safe => 2 }
}

// @decl identity | identity
pub fn identity<T>(value: T) -> T {
    value
}

// @decl run | run
pub fn run() -> String {
    // @expr new | Config::new(3)
    let config = Config::new(3);
    // @expr doubled | config.doubled()
    let count = config.doubled();
    // @expr identity | identity(count)
    let count = identity(count);
    // @expr choose | choose(Mode::Fast)
    let offset = choose(Mode::Fast);
    // @expr count | count + offset
    let sum = count + offset;
    // @expr map | (0..sum).map(|n| n * 2).collect::<Vec<_>>()
    let values = (0..sum).map(|n| n * 2).collect::<Vec<_>>();
    // @expr label | config.label()
    let label = config.label();
    // @expr format | format!("{label}: {}", values.len())
    format!("{label}: {}", values.len())
}

// @decl alias | alias
pub mod alias {
    pub use super::Config;
}

#[cfg(test)]
mod tests {
    use super::*;

    // @decl smoke | smoke
    #[test]
    fn smoke() {
        // @expr run | run()
        assert!(!run().is_empty());
    }
}
