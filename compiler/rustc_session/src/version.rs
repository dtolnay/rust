use rustc_data_structures::stable_hasher::{HashStable, StableHasher};
use std::fmt::{self, Display};

#[derive(Encodable, Decodable, Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
//#[derive(HashStable_Generic)]
pub struct RustcVersion {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
}

impl RustcVersion {
    pub const CURRENT: Self = current_rustc_version!(env!("CFG_RELEASE"));
}

impl Display for RustcVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

// Handwritten because #[derive(HashStable_Generic)] would generate a stricter
// `Ctx: rustc_session::HashStableContext` bound.
impl<Ctx> HashStable<Ctx> for RustcVersion
where
    Ctx: rustc_ast::HashStableContext,
{
    fn hash_stable(&self, hcx: &mut Ctx, hasher: &mut StableHasher) {
        self.major.hash_stable(hcx, hasher);
        self.minor.hash_stable(hcx, hasher);
        self.patch.hash_stable(hcx, hasher);
    }
}
