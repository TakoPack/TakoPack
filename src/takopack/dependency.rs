use cargo::core::Dependency;
use itertools::Itertools;

use std::cmp;
use std::fmt;

use crate::config::testing_ignore_debpolv;
use crate::errors::*;
use crate::takopack::{self, control::base_deb_name, Package};

#[derive(Eq, Clone)]
#[allow(clippy::upper_case_acronyms)]
enum V {
    M(u64),
    MM(u64, u64),
    MMP(u64, u64, u64),
    // For prerelease versions like 0.26.0-beta.1
    Prerelease(u64, u64, u64, String),
}

impl V {
    fn new(p: &semver::Comparator) -> Result<Self> {
        use self::V::*;

        // Check if this has a prerelease part
        if !p.pre.is_empty() {
            let major = p.major;
            let minor = p.minor.unwrap_or(0);
            let patch = p.patch.unwrap_or(0);
            let pre = p.pre.to_string();
            return Ok(Prerelease(major, minor, patch, pre));
        }

        let mmp = match (p.minor, p.patch) {
            (None, None) => M(p.major),
            (Some(minor), None) => MM(p.major, minor),
            (Some(minor), Some(patch)) => MMP(p.major, minor, patch),
            (None, Some(_)) => takopack_bail!("semver had patch without minor"),
        };
        Ok(mmp)
    }

    fn inclast(&self) -> V {
        use self::V::*;
        match *self {
            M(major) => M(major + 1),
            MM(major, minor) => MM(major, minor + 1),
            MMP(major, minor, patch) => MMP(major, minor, patch + 1),
            Prerelease(major, minor, patch, ref pre) => {
                // For prerelease versions, increment patch and keep prerelease
                Prerelease(major, minor, patch + 1, pre.clone())
            }
        }
    }

    fn mmp(&self) -> (u64, u64, u64) {
        use self::V::*;
        match *self {
            M(major) => (major, 0, 0),
            MM(major, minor) => (major, minor, 0),
            MMP(major, minor, patch) => (major, minor, patch),
            Prerelease(major, minor, patch, _) => (major, minor, patch),
        }
    }
}

impl Ord for V {
    fn cmp(&self, other: &V) -> cmp::Ordering {
        self.mmp().cmp(&other.mmp())
    }
}

impl PartialOrd for V {
    fn partial_cmp(&self, other: &V) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for V {
    fn eq(&self, other: &V) -> bool {
        self.mmp() == other.mmp()
    }
}

impl fmt::Display for V {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use self::V::*;
        match *self {
            M(major) => write!(f, "{}", major),
            MM(major, minor) => write!(f, "{}.{}", major, minor),
            MMP(major, minor, patch) => write!(f, "{}.{}.{}", major, minor, patch),
            Prerelease(major, minor, patch, ref pre) => {
                write!(f, "{}.{}.{}-{}", major, minor, patch, pre)
            }
        }
    }
}

struct VRange {
    ge: Option<V>,
    lt: Option<V>,
}

impl VRange {
    fn new() -> Self {
        VRange { ge: None, lt: None }
    }

    fn constrain_ge(&mut self, ge: V) -> &Self {
        match self.ge {
            Some(ref ge_) if &ge < ge_ => (),
            _ => self.ge = Some(ge),
        };
        self
    }

    fn constrain_lt(&mut self, lt: V) -> &Self {
        match self.lt {
            Some(ref lt_) if &lt >= lt_ => (),
            _ => self.lt = Some(lt),
        };
        self
    }

    fn to_deb_clause(&self, base: &str, suffix: &str) -> Result<Vec<String>> {
        use takopack::dependency::V::*;
        match (&self.ge, &self.lt) {
            (None, None) => Ok(vec![format!("{}{}", base, suffix)]),
            (Some(ge), None) => Ok(vec![format!("{}{} (>= {}-~~)", base, suffix, ge)]),
            (None, Some(lt)) => Ok(vec![format!("{}{} (<< {}-~~)", base, suffix, lt)]),
            (Some(ge), Some(lt)) => {
                if ge >= lt {
                    takopack_bail!("bad version range: >= {}, << {}", ge, lt);
                }
                let mut ranges = vec![];
                let (lt_maj, lt_min, lt_pat) = lt.mmp();
                let (ge_maj, ge_min, _ge_pat) = ge.mmp();

                if ge_maj + 1 == lt_maj && lt_min == 0 && lt_pat == 0 {
                    // upper bound doesn't restrict lower bound further if we include the major
                    // part
                    ranges.push((Some(M(ge_maj)), true, ge));
                } else if ge_maj < lt_maj {
                    // different major versions, unversioned package needs to satisfy
                    ranges.push((None, true, ge));
                    ranges.push((None, false, lt));
                } else {
                    assert_eq!(ge_maj, lt_maj);
                    if ge_maj == 0 && ge_min + 1 == lt_min && lt_pat == 0 {
                        // upper bound doesn't restrict lower bound further if we include 0.X
                        ranges.push((Some(MM(ge_maj, ge_min)), true, ge));
                    } else if ge_maj == 0 && ge_min < lt_min {
                        // different 0.X versions, unversioned package needs to satisfy
                        ranges.push((None, true, ge));
                        ranges.push((None, false, lt));
                    } else if ge_min < lt_min {
                        // different minor versions within a major version, any package with the
                        // corresponding major version can potentially satisfy
                        ranges.push((Some(M(ge_maj)), true, ge));
                        ranges.push((Some(M(lt_maj)), false, lt));
                    } else {
                        // just the patch level differs, but both ends restricted
                        // any package with the corresponding major.minor version can potentially
                        // satisfy
                        assert_eq!(ge_min, lt_min);
                        ranges.push((Some(MM(ge_maj, ge_min)), true, ge));
                        ranges.push((Some(MM(lt_maj, lt_min)), false, lt));
                    }
                };
                // unversioned package name is only provided by the non semver-suffixed packages
                // if a range is only satisfiable by semver-suffixed variants in the archive, it
                // needs to be collapsed/reduced accordingly
                Ok(ranges
                    .iter()
                    .filter_map(|(ver, greater, cons)| match (ver, greater, cons) {
                        (None, true, c) => Some(format!("{}{} (>= {}-~~)", base, suffix, c)),
                        (None, false, c) => Some(format!("{}{} (<< {}-~~)", base, suffix, c)),
                        (Some(ver), true, c) => {
                            if c == &ver {
                                // A-x >= x is redundant, drop the >=
                                Some(format!("{}-{}{}", base, ver, suffix))
                            } else {
                                Some(format!("{}-{}{} (>= {}-~~)", base, ver, suffix, c))
                            }
                        }
                        (Some(ver), false, c) => {
                            if c == &ver {
                                // A-x << x is unsatisfiable, drop it
                                None
                            } else {
                                Some(format!("{}-{}{} (<< {}-~~)", base, ver, suffix, c))
                            }
                        }
                    })
                    .collect())
            }
        }
    }
}

fn coerce_unacceptable_predicate<'a>(
    dep: &Dependency,
    p: &'a semver::Comparator,
    allow_prerelease_deps: bool,
) -> Result<&'a semver::Op> {
    let mmp = &V::new(p)?;

    // Cargo/semver and takopack handle pre-release versions quite
    // differently, so a versioned takopack dependency cannot properly
    // handle pre-release crates. This might be OK most of the time,
    // coerce it to the non-pre-release version.
    if !p.pre.is_empty() {
        // For dependencies with prerelease versions (e.g., 0.26.0-beta.1),
        // we allow them and will record the full version including prerelease part
        takopack_warn!(
            "Dependency has prerelease version, will use full version: {} {:?}",
            dep.package_name(),
            p
        )
    }

    use semver::Op::*;
    use takopack::dependency::V::M;
    match (&p.op, mmp) {
        (&Greater, &M(0)) => Ok(&p.op),
        (&GreaterEq, &M(0)) => {
            takopack_warn!(
                "Coercing unrepresentable dependency version predicate 'GtEq 0' to 'Gt 0': {} {:?}",
                dep.package_name(),
                p
            );
            Ok(&Greater)
        }
        // TODO: This will prevent us from handling wildcard dependencies with
        // 0.0.0* so for now commenting this out.
        // (_, &M(0)) => takopack_bail!(
        //     "Unrepresentable dependency version predicate: {} {:?}",
        //     dep.package_name(),
        //     p
        // ),
        (_, _) => Ok(&p.op),
    }
}

fn generate_version_constraints(
    vr: &mut VRange,
    dep: &Dependency,
    p: &semver::Comparator,
    op: &semver::Op,
) -> Result<()> {
    let mmp = V::new(p)?;
    use semver::Op::*;
    use takopack::dependency::V::*;
    // see https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html
    // and https://docs.rs/semver/1/semver/enum.Op.html for semantics
    match (*op, &mmp) {
        (Less, &M(0)) | (Less, &MM(0, 0)) | (Less, &MMP(0, 0, 0)) => takopack_bail!(
            "Unrepresentable dependency version predicate: {} {:?}",
            dep.package_name(),
            p
        ),
        (Less, _) => {
            vr.constrain_lt(mmp);
        }
        (LessEq, _) => {
            vr.constrain_lt(mmp.inclast());
        }
        (Greater, _) => {
            vr.constrain_ge(mmp.inclast());
        }
        (GreaterEq, _) => {
            vr.constrain_ge(mmp);
        }
        (Exact, _) | (Wildcard, _) => {
            vr.constrain_lt(mmp.inclast());
            vr.constrain_ge(mmp);
        }
        (Tilde, &M(_)) | (Tilde, &MM(_, _)) => {
            vr.constrain_lt(mmp.inclast());
            vr.constrain_ge(mmp);
        }
        (Tilde, &MMP(major, minor, _)) => {
            vr.constrain_lt(MM(major, minor + 1));
            vr.constrain_ge(mmp);
        }

        (Caret, &MMP(0, 0, _)) => {
            vr.constrain_lt(mmp.inclast());
            vr.constrain_ge(mmp);
        }
        (Caret, &MMP(0, minor, _)) | (Caret, &MM(0, minor)) => {
            vr.constrain_lt(MM(0, minor + 1));
            vr.constrain_ge(mmp);
        }
        (Caret, &MMP(major, _, _)) | (Caret, &MM(major, _)) | (Caret, &M(major)) => {
            vr.constrain_lt(M(major + 1));
            vr.constrain_ge(mmp);
        }
        // Handle Prerelease versions with Caret operator
        (Caret, &Prerelease(0, 0, _, _)) => {
            vr.constrain_lt(mmp.inclast());
            vr.constrain_ge(mmp);
        }
        (Caret, &Prerelease(0, minor, _, _)) => {
            vr.constrain_lt(MM(0, minor + 1));
            vr.constrain_ge(mmp);
        }
        (Caret, &Prerelease(major, _, _, _)) => {
            vr.constrain_lt(M(major + 1));
            vr.constrain_ge(mmp);
        }
        // Handle Prerelease versions with Tilde operator
        (Tilde, &Prerelease(major, minor, _, _)) => {
            vr.constrain_lt(MM(major, minor + 1));
            vr.constrain_ge(mmp);
        }

        (_, _) => {
            // https://github.com/dtolnay/semver/issues/262
            panic!("Op is non-exhaustive for some reason");
        }
    }

    Ok(())
}

/// Translates a Cargo dependency into a takopack package dependency.
pub fn deb_dep(allow_prerelease_deps: bool, dep: &Dependency) -> Result<Vec<String>> {
    // println!("{:?}",dep.package_name());
    let dep_dashed = base_deb_name(&dep.package_name());
    let mut suffixes = Vec::new();
    if dep.uses_default_features() {
        suffixes.push("+default-dev".to_string());
    }
    for feature in dep.features() {
        suffixes.push(format!("+{}-dev", base_deb_name(feature)));
    }
    if suffixes.is_empty() {
        suffixes.push("-dev".to_string());
    }
    let req = semver::VersionReq::parse(&dep.version_req().to_string())?;
    let mut deps = Vec::new();
    for suffix in suffixes {
        let base = format!("{}-{}", Package::pkg_prefix(), dep_dashed);
        let mut vr = VRange::new();
        for p in &req.comparators {
            let op = coerce_unacceptable_predicate(dep, p, allow_prerelease_deps)?;
            generate_version_constraints(&mut vr, dep, p, op)?;
        }
        deps.extend(vr.to_deb_clause(&base, &suffix)?);
    }
    Ok(deps)
}

pub fn deb_deps(allow_prerelease_deps: bool, cdeps: &[Dependency]) -> Result<Vec<String>> // result is an AND-clause
{
    let mut deps = Vec::new();
    // let mut i = 0;
    for dep in cdeps {
        // println!(" dep {:?}", dep);
        deps.extend(
            deb_dep(allow_prerelease_deps, dep)?
                .iter()
                .map(String::to_string),
        );
        // println!("deps {}", deps[i]);
        // i  = i+1;
    }
    deps.sort();
    deps.dedup();
    Ok(deps)
}

pub fn deb_dep_add_nocheck(x: &str) -> String {
    x.split('|')
        .map(|x| x.trim_end().to_string() + " <!nocheck> ")
        .join("|")
        .trim_end()
        .to_string()
}
