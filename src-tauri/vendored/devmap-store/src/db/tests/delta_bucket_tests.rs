use super::*;

/// An identity that digests the same as every other, however different it
/// is.
///
/// Real SipHash collisions cannot be summoned on demand, and a test that
/// injected its own digest would no longer be testing the digest the write
/// path uses. This hashes to a constant instead, so `identity_digest` — the
/// one function both the bucketing and the search go through — returns the
/// same value for every value of it, and the collision the write path meets
/// once in a very long while is here every time.
#[derive(PartialEq, Eq, Debug)]
struct Collides(&'static str);

impl std::hash::Hash for Collides {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        state.write_u8(0);
    }
}

/// A collision must cost a comparison, never an answer.
///
/// [`claim_matching_candidate`] narrows with a 64-bit digest and decides
/// with `PartialEq`. Were it to trust the digest, two different rows that
/// happened to digest alike would be treated as one: the live row left
/// open, the new row never written, and the generation reading back an edge
/// it was never given.
#[test]
fn a_collision_narrows_the_search_and_never_decides_it() {
    let names = ["alpha", "beta", "gamma"];
    let (buckets, chain) = bucket_identities(names.len(), |index| Some(Collides(names[index])));
    assert_eq!(
        buckets.len(),
        1,
        "the fixture only tests collisions if the identities actually collide"
    );

    let mut matched = vec![false; names.len()];
    let claim = |live: &'static str, matched: &mut Vec<bool>| {
        claim_matching_candidate(&buckets, &chain, matched, &Collides(live), |index| {
            Some(Collides(names[index]))
        })
    };

    assert!(claim("beta", &mut matched), "beta is one of the candidates");
    assert_eq!(
        matched,
        vec![false, true, false],
        "the candidate claimed is the one that compared equal, not the one \
             the bucket happened to offer first"
    );
    assert!(
        !claim("delta", &mut matched),
        "a row nothing equals is not in this generation, however it digests"
    );
    assert_eq!(
        matched,
        vec![false, true, false],
        "a search that found nothing claims nothing"
    );
    assert!(claim("alpha", &mut matched));
    assert!(claim("gamma", &mut matched));
    assert!(
        !claim("alpha", &mut matched),
        "each candidate is claimed once, so a fourth live row finds none"
    );
}

/// A repeated row is repeated candidates, not one candidate with a count.
///
/// 475 edge tuples of this repository occur more than once in a single
/// generation. If the delta collapsed them, a rebuild would close the
/// copies it could not account for and the generation would lose rows the
/// analysis counted.
#[test]
fn a_row_stored_three_times_answers_three_live_rows_and_no_more() {
    let names = ["duplicate", "duplicate", "duplicate"];
    let (buckets, chain) = bucket_identities(names.len(), |index| Some(names[index]));
    let mut matched = vec![false; names.len()];
    let claim = |matched: &mut Vec<bool>| {
        claim_matching_candidate(&buckets, &chain, matched, &"duplicate", |index| {
            Some(names[index])
        })
    };

    assert!(claim(&mut matched));
    assert!(claim(&mut matched));
    assert!(claim(&mut matched));
    assert_eq!(matched, vec![true, true, true], "all three were claimed");
    assert!(
        !claim(&mut matched),
        "a fourth live copy has no candidate left, so it is closed"
    );
}

/// An index outside this generation is a candidate for nothing.
///
/// Edges touching a deleted path are not part of the generation, so their
/// identity is `None`. Two things keep them out, and this asserts both:
/// they enter no bucket and no chain, so nothing can offer them; and the
/// search skips them even if something did. Either alone would hold open a
/// row this generation does not contain the day the other changed.
#[test]
fn an_index_outside_the_generation_is_never_claimed() {
    let names = [Some("kept"), None, Some("kept")];
    let (buckets, chain) = bucket_identities(names.len(), |index| names[index]);
    assert!(
        !buckets
            .values()
            .chain(chain.iter())
            .any(|&index| index == 1),
        "an index with no identity is in no bucket and on no chain: {buckets:?} {chain:?}"
    );
    let mut matched = vec![false; names.len()];
    let claim = |matched: &mut Vec<bool>| {
        claim_matching_candidate(&buckets, &chain, matched, &"kept", |index| names[index])
    };

    assert!(claim(&mut matched));
    assert!(claim(&mut matched));
    assert_eq!(
        matched,
        vec![true, false, true],
        "the excluded index is still unmatched, because it was never a candidate"
    );
    assert!(!claim(&mut matched), "there is no third candidate");
}
