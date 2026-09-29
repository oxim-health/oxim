//! Synthetic messages. Every value is invented; nothing resembles a real
//! patient. A seed makes runs reproducible.

use std::fmt::Write as _;

const FAMILY: &[&str] = &["Test", "Sample", "Demo", "Synthetic", "Example"];
const GIVEN: &[&str] = &["Alex", "Kim", "Sam", "Robin", "Charlie", "Deniz"];
/// (code, name, unit, low, high, decimals)
const TESTS: &[(&str, &str, &str, f64, f64, usize)] = &[
    ("GLU", "Glucose", "mmol/L", 3.9, 6.1, 1),
    ("CREA", "Creatinine", "umol/L", 45.0, 110.0, 0),
    ("HGB", "Hemoglobin", "g/dL", 12.0, 17.0, 1),
    ("WBC", "Leukocytes", "10*9/L", 4.0, 10.0, 2),
    ("K", "Potassium", "mmol/L", 3.5, 5.1, 1),
    ("NA", "Sodium", "mmol/L", 135.0, 145.0, 0),
];

/// A deterministic generator of synthetic messages.
#[derive(Debug, Clone)]
pub struct Generator {
    rng: fastrand::Rng,
    counter: u64,
}

/// One synthetic result.
#[derive(Debug, Clone, PartialEq)]
pub struct SyntheticResult {
    /// Local test code.
    pub code: &'static str,
    /// Test name.
    pub name: &'static str,
    /// Value as text with the test's precision.
    pub value: String,
    /// Unit.
    pub unit: &'static str,
    /// Reference range as `low-high`.
    pub range: String,
    /// `H`, `L` or `N`.
    pub flag: &'static str,
}

impl Generator {
    /// A generator with a fixed seed.
    pub fn new(seed: u64) -> Self {
        Self {
            rng: fastrand::Rng::with_seed(seed),
            counter: 0,
        }
    }

    fn next_id(&mut self) -> u64 {
        self.counter += 1;
        self.counter
    }

    fn pick<'a>(&mut self, items: &'a [&'a str]) -> &'a str {
        items[self.rng.usize(..items.len())]
    }

    /// A few results with values around their reference ranges.
    pub fn results(&mut self, count: usize) -> Vec<SyntheticResult> {
        let mut picked: Vec<_> = TESTS.to_vec();
        self.rng.shuffle(&mut picked);
        picked
            .into_iter()
            .take(count.clamp(1, TESTS.len()))
            .map(|(code, name, unit, low, high, decimals)| {
                let span = high - low;
                let value = low - span * 0.2 + self.rng.f64() * span * 1.4;
                let flag = if value < low {
                    "L"
                } else if value > high {
                    "H"
                } else {
                    "N"
                };
                SyntheticResult {
                    code,
                    name,
                    value: format!("{value:.decimals$}"),
                    unit,
                    range: format!("{low:.decimals$}-{high:.decimals$}"),
                    flag,
                }
            })
            .collect()
    }

    /// An HL7 v2.5.1 ORU^R01 result message with CR segment endings.
    pub fn hl7_oru(&mut self, results: usize) -> Vec<u8> {
        let id = self.next_id();
        let family = self.pick(FAMILY);
        let given = self.pick(GIVEN);
        let patient = 100_000 + self.rng.u32(..900_000);
        let mut text = format!(
            "MSH|^~\\&|OXIM-SIM|LAB|LIS|HOSP|20260929120000||ORU^R01^ORU_R01|SIM{id:06}|P|2.5.1\r\
             PID|1||{patient}^^^HOSP^MR||{family}^{given}||19800101|U\r\
             OBR|1|ORD{id:06}|SMP{id:06}|PANEL^Synthetic panel^L|||20260929115500\r"
        );
        for (index, result) in self.results(results).iter().enumerate() {
            let _ = write!(
                text,
                "OBX|{}|NM|{}^{}^L||{}|{}|{}|{}|||F|||20260929115900\r",
                index + 1,
                result.code,
                result.name,
                result.value,
                result.unit,
                result.range,
                result.flag
            );
        }
        text.into_bytes()
    }

    /// An ASTM E1394 result message (H, P, O, R..., L) with CR record
    /// endings, as an analyzer sends it.
    pub fn astm_results(&mut self, results: usize) -> Vec<u8> {
        let id = self.next_id();
        let family = self.pick(FAMILY);
        let given = self.pick(GIVEN);
        let mut text = format!(
            "H|\\^&|||OXIM-SIM^1.0|||||||P|LIS2-A2|20260929120000\r\
             P|1||PAT{id:06}||{family}^{given}||19800101|U\r\
             O|1|SMP{id:06}||^^^PANEL|R||||||N||||SERUM\r"
        );
        for (index, result) in self.results(results).iter().enumerate() {
            let _ = write!(
                text,
                "R|{}|^^^{}|{}|{}|{}|{}||F||SIM||20260929115900|OXIM-SIM\r",
                index + 1,
                result.code,
                result.value,
                result.unit,
                result.range,
                result.flag
            );
        }
        text.push_str("L|1|N\r");
        text.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_messages_parse() {
        let mut generator = Generator::new(7);
        for _ in 0..50 {
            let oru = generator.hl7_oru(4);
            let message = oxim_hl7::Message::parse(&oru).unwrap();
            assert_eq!(message.segments_named("OBX").count(), 4);
            let astm = generator.astm_results(3);
            let message = oxim_astm::Message::parse(&astm).unwrap();
            assert!(message.is_terminated());
        }
    }

    #[test]
    fn generation_is_reproducible() {
        assert_eq!(Generator::new(1).hl7_oru(3), Generator::new(1).hl7_oru(3));
    }
}
