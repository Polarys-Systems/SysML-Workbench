//! A compact standard library, embedded in the crate: the packages of the
//! SysML v2 Systems Model Library that models import most (`ScalarValues`,
//! `ISQ`, `SI`, the base definitions of parts, ports, actions, requirements,
//! ...), so their names resolve. Declarations only, parsed once.
//!
//! ```
//! let model = sysml::parse("package P { private import ISQ::*; attribute m : MassValue; }").model;
//! let mut models = vec![&model];
//! models.extend(sysml::library::files().iter().map(|f| &f.model));
//! let index = sysml::Index::build_many(&models);
//! assert!(index.get("ISQ::MassValue").is_some());
//! ```

use crate::Model;
use std::sync::OnceLock;

pub struct LibraryFile {
    /// File name, e.g. `ISQ.sysml`.
    pub name: &'static str,
    pub source: &'static str,
    pub model: Model,
}

const SOURCES: [(&str, &str); 4] = [
    ("Base.sysml", include_str!("../library/Base.sysml")),
    ("ScalarValues.sysml", include_str!("../library/ScalarValues.sysml")),
    ("ISQ.sysml", include_str!("../library/ISQ.sysml")),
    ("SI.sysml", include_str!("../library/SI.sysml")),
];

/// The library files, parsed on first use.
pub fn files() -> &'static [LibraryFile] {
    static FILES: OnceLock<Vec<LibraryFile>> = OnceLock::new();
    FILES.get_or_init(|| {
        SOURCES.iter().map(|&(name, source)| LibraryFile { name, source, model: crate::parse(source).model }).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Index, Name, QualifiedName, Span};

    fn qn(s: &str) -> QualifiedName {
        let segments = s.split("::").map(|t| Name { text: t.to_string(), span: Span::default() }).collect();
        QualifiedName { segments, span: Span::default() }
    }

    #[test]
    fn library_parses_cleanly_and_resolves_common_names() {
        for (name, src) in SOURCES {
            let r = crate::parse(src);
            assert!(r.diagnostics.is_empty(), "{name}: {:?}", r.diagnostics);
        }
        let user = crate::parse(
            "package P {
                private import ScalarValues::*;
                private import ISQ::*;
                part def Car { attribute m : MassValue; attribute n : Integer; attribute t : TimeValue; }
            }",
        )
        .model;
        let mut models = vec![&user];
        models.extend(files().iter().map(|f| &f.model));
        let index = Index::build_many(&models);
        let car = index.get("P::Car").unwrap();
        let resolve = |s: &str| index.resolve(Some(car), &qn(s)).map(|id| index.entry(id).qualified_name.clone());
        assert_eq!(resolve("MassValue").as_deref(), Some("ISQ::MassValue"));
        assert_eq!(resolve("Integer").as_deref(), Some("ScalarValues::Integer"));
        // Aliases and unit symbols (short names).
        assert_eq!(resolve("TimeValue").as_deref(), Some("ISQ::DurationValue"));
        assert_eq!(resolve("SI::kg").as_deref(), Some("SI::kilogram"));
        assert_eq!(resolve("Parts::Part").as_deref(), Some("Parts::Part"));
        assert!(index.entries().iter().filter(|e| e.source > 0).count() > 200);
    }
}
