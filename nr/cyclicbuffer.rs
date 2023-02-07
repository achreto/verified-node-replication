#[allow(unused_imports)]
use builtin::*;
use builtin_macros::*;

use super::pervasive::map::*;
use super::pervasive::seq::*;
use super::pervasive::set::*;
use super::pervasive::*;

use state_machines_macros::*;

use super::types::*;
use super::utils::*;


////////////////////////////////////////////////////////////////////////////////////////////////////
//
// Cyclic Buffer
// =============
//
// Dafny: https://github.com/secure-foundations/iron-sync/blob/concurrency-experiments/concurrency/node-replication/CyclicBuffer.i.dfy
////////////////////////////////////////////////////////////////////////////////////////////////////


////////////////////////////////////////////////////////////////////////////////////////////////////
//
////////////////////////////////////////////////////////////////////////////////////////////////////




// rust_verify/tests/example.rs ignore



type Key = int;

pub struct StoredType { } // TODO

verus!{
    pub spec fn stored_type_inv(st: StoredType, idx: int) -> bool;
}


////////////////////////////////////////////////////////////////////////////////////////////////////
// Read Only Operations
////////////////////////////////////////////////////////////////////////////////////////////////////

///
#[is_variant]
pub enum ReaderState {
    ///
    Starting {
        ///
        start: LogIdx
    },
    /// reader in the range
    Range { start: LogIdx, end: LogIdx, cur: LogIdx },
    /// Guard
    Guard { start: LogIdx, end: LogIdx, cur: LogIdx, val: StoredType },
}


////////////////////////////////////////////////////////////////////////////////////////////////////
// Combiner
////////////////////////////////////////////////////////////////////////////////////////////////////

/// represents the combiner
#[is_variant]
pub enum CombinerState {
    Idle,
    Reading(ReaderState),
    AdvancingHead{idx: LogIdx, min_head: LogIdx},
    AdvancingTail{observed_head: LogIdx},
    Appending{cur_idx: LogIdx, tail: LogIdx},
}


tokenized_state_machine!{ CyclicBuffer {
    fields {
        /// the size of the buffer
        #[sharding(constant)]
        pub buffer_size: LogIdx,

        /// the number of replicas
        #[sharding(constant)]
        pub num_replicas: nat,

        // Logical index into the above slice at which the log starts.
        // Note: the head does _not_ necessarily advance monotonically.
        // (It could go backwards in the event of two threads overlapping
        // in their AdvancingHead cycles.)
        // It's only guaranteed to be <= all the local heads.

        #[sharding(variable)]
        pub head: LogIdx,

        // Logical index into the above slice at which the log ends.
        // New appends go here.

        #[sharding(variable)]
        pub tail: LogIdx,

        // Array consisting of the local head of each replica registered with the log.
        // Required for garbage collection; since replicas make progress over the log
        // independently, we want to make sure that we don't garbage collect operations
        // that haven't been executed by all replicas.

        #[sharding(map)]
        pub local_heads: Map<NodeId, LogIdx>,    // previously called local_tails

        /// the contents of the buffer/log.
        #[sharding(storage_map)]
        pub contents: Map<int, StoredType>,

        // The 'alive' bit flips back and forth. So sometimes 'true' means 'alive',
        // and sometimes 'false' means 'alive'.
        // entry is an index into the buffer (0 <= entry < LOG_SIZE)

        #[sharding(map)]
        pub alive_bits: Map</* entry: */ LogIdx, /* bit: */ bool>,

        #[sharding(map)]
        pub combiner_state: Map<NodeId, CombinerState>
    }


    ////////////////////////////////////////////////////////////////////////////////////////////
    // Invariant
    ////////////////////////////////////////////////////////////////////////////////////////////


    #[invariant]
    pub spec fn complete(&self) -> bool {
        &&& (forall |i| 0 <= i < self.num_replicas <==>
            self.local_heads.dom().contains(i))
        &&& (forall |i| 0 <= i < self.buffer_size <==>
            self.alive_bits.dom().contains(i))
        &&& (forall |i| 0 <= i < self.num_replicas <==>
            self.combiner_state.dom().contains(i))

        &&& (forall |i|
            self.contents.dom().contains(i) ==> -self.buffer_size <= i < self.tail)
    }

    #[invariant]
    pub spec fn pointer_ordering(&self) -> bool {
        &&& self.head <= self.tail
        &&& (forall |i| #[trigger] self.local_heads.dom().contains(i) ==>
            self.head <= self.local_heads.index(i) <= self.tail)
        &&& (forall |i| #[trigger] self.local_heads.dom().contains(i) ==>
            self.tail - self.buffer_size <= self.local_heads.index(i))
    }

    #[invariant]
    pub spec fn pointer_differences(&self) -> bool {
        forall |i| self.local_heads.dom().contains(i) ==>
            self.local_heads.index(i)
            <= self.tail
            <= self.local_heads.index(i) + self.buffer_size
    }

    #[invariant]
    pub spec fn ranges_no_overlap(&self) -> bool {
        (forall |i, j| self.combiner_state.dom().contains(i) && self.combiner_state.dom().contains(j) && i != j ==>
            match self.combiner_state.index(i) {
                CombinerState::Appending{cur_idx, tail} => {
                    match self.combiner_state.index(j) {
                        CombinerState::Reading(ReaderState::Guard{start, end, cur, val}) => {
                            cur_idx > cur || tail <= cur
                        }
                        CombinerState::Appending{cur_idx: cur_idx2, tail: tail2} => {
                            cur_idx <= tail2 || tail <= cur_idx2
                        }
                        _ => { true }
                    }
                }
                _ => { true }
            }
        )
    }

    #[invariant]
    pub spec fn upcoming_bits_are_not_alive(&self) -> bool {
        let min_local_head = map_min_value(self.local_heads, (self.num_replicas - 1) as nat);
        forall |i|
            self.tail <= i < min_local_head + self.buffer_size
            ==> !entry_is_alive(self.alive_bits, i, self.buffer_size)
    }

    #[invariant]
    pub spec fn inv_buffer_contents(&self) -> bool {
        &&& (forall |i: int| self.tail - self.buffer_size <= i < self.tail ==> (
            (entry_is_alive(self.alive_bits, i, self.buffer_size) ||
                i < map_min_value(self.local_heads, (self.num_replicas - 1) as nat))
            <==>
            #[trigger] self.contents.dom().contains(i)
        ))
        &&& (forall |i: int| self.tail <= i ==> ! #[trigger] self.contents.dom().contains(i))
    }

    #[invariant]
    pub spec fn contents_meet_inv(&self) -> bool {
        forall |i: int| #[trigger] self.contents.dom().contains(i) ==>
            stored_type_inv(self.contents[i], i)
    }

    #[invariant]
    pub spec fn all_reader_state_valid(&self) -> bool {
        forall |node_id| #[trigger] self.combiner_state.dom().contains(node_id) && self.combiner_state[node_id].is_Reading() ==>
          self.reader_state_valid(node_id, self.combiner_state[node_id].get_Reading_0())
    }

    pub closed spec fn reader_state_valid(&self, node_id: NodeId, rs: ReaderState) -> bool {
        match rs {
            ReaderState::Starting{start} => {
                // the starting value should match the local tail
                &&& start == self.local_heads[node_id]
            }
            ReaderState::Range{start, end, cur} => {
                // the start must be our local tail
                &&& self.local_heads[node_id] == start
                // the start must be before the end, can be equial if ltail == gtail
                &&& start <= end
                // we've read the tail, but the tail may have moved
                &&& (self.tail as int) - (self.buffer_size as int) <= end <= (self.tail as int)
                // current is between start and end
                &&& start <= cur <= end
                // the entries up to, and including  current must be alive
                &&& (forall |i| start <= i < cur ==> (
                  entry_is_alive(self.alive_bits, i, self.buffer_size)))
                // the entries up to, and including current must have something in the log
                &&& (forall |i| start <= i < cur ==> self.contents.dom().contains(i))
            }
            ReaderState::Guard{start, end, cur, val} => {
                // the start must be our local tail
                &&& self.local_heads[node_id] == start
                // the start must be before the end, can be equial if ltail == gtail
                &&& start <= end
                // we've read the tail, but the tail may have moved
                &&& (self.tail as int) - (self.buffer_size as int) <= end <= (self.tail as int)
                // current is between start and end
                &&& start <= cur < end
                // the entries up to, and including  current must be alive
                &&& (forall |i| start <= i <= cur ==> (
                  entry_is_alive(self.alive_bits, i, self.buffer_size)))
                // the entries up to, and including current must have something in the log
                &&& (forall |i| start <= i <= cur ==> self.contents.dom().contains(i))
                // the thing we are ready should match the log content
                &&& self.contents.dom().contains(cur as int)
                &&& self.contents[cur as int] === val
            }
        }
    }

    #[invariant]
    pub spec fn all_combiner_state_valid(&self) -> bool {
        forall |node_id| #[trigger] self.combiner_state.dom().contains(node_id) ==>
          self.combiner_state_valid(node_id, self.combiner_state[node_id])
    }

    pub closed spec fn combiner_state_valid(&self, node_id: NodeId, cs: CombinerState) -> bool {
        match cs {
            CombinerState::Idle => true,
            CombinerState::Reading(_) => true, // see reader_state_valid instead
            CombinerState::AdvancingHead{idx, min_head} => {
                // the index is always within the defined replicas
                &&& idx <= self.num_replicas as nat
                // forall replicas we'e seen, min_head is smaller than all localTails
                &&& (forall |n| 0 <= n < idx ==> min_head <= (
                  self.local_heads[n]))
            }
            CombinerState::AdvancingTail{observed_head} => {
                // the observed head is smaller than all local tails
                &&& (forall |n| 0 <= n < self.num_replicas as nat ==> observed_head <= (
                  self.local_heads[n]))
            }
            CombinerState::Appending{cur_idx, tail} => {
                // the current index is between local tails and tail.
                &&& self.local_heads[node_id] <= cur_idx <= tail
                // the read tail is smaller or equal to the current tail.
                &&& tail <= self.tail
                //
                &&& (self.tail as int) - (self.buffer_size as int) <= cur_idx <= (self.tail as int)
                // all the entries we're writing must not be alive.
                &&& (forall |i : nat| cur_idx <= i < tail ==> (
                  !(entry_is_alive(self.alive_bits, i as int, self.buffer_size))))
            }
        }
    }


    ////////////////////////////////////////////////////////////////////////////////////////////
    // Initialization
    ////////////////////////////////////////////////////////////////////////////////////////////

    init!{
        initialize(buffer_size: nat, num_replicas: nat, contents: Map<int, StoredType>) {
            init buffer_size = buffer_size;
            init num_replicas = num_replicas;
            init head = 0;
            init tail = 0;
            init local_heads = Map::new(|i: NodeId| 0 <= i < num_replicas, |i: NodeId| 0);

            require(forall |i: int| (-buffer_size <= i < 0 <==> contents.dom().contains(i)));
            init contents = contents;

            init alive_bits = Map::new(|i: nat| 0 <= i < buffer_size, |i: nat| false);
            init combiner_state = Map::new(|i: NodeId| 0 <= i < num_replicas, |i: NodeId| CombinerState::Idle);
        }
    }

    ////////////////////////////////////////////////////////////////////////////////////////////
    // Readonly Transitions
    ////////////////////////////////////////////////////////////////////////////////////////////

    transition!{
        reader_do_start(node_id: NodeId) {
            have   local_heads    >= [ node_id => let local_head ];

            remove combiner_state -= [ node_id => CombinerState::Idle ];
            add    combiner_state += [ node_id => CombinerState::Reading(ReaderState::Starting { start: local_head }) ];
        }
    }

    transition!{
        reader_do_enter(node_id: NodeId) {
            remove combiner_state -= [ node_id =>
                let CombinerState::Reading(ReaderState::Starting {
                    start,
                })
            ];
            add combiner_state += [ node_id =>
                CombinerState::Reading(
                    ReaderState::Range{
                        start: start,
                        end: pre.tail,
                        cur: start,
                    },
                )
            ];
        }
    }

    transition!{
        reader_do_guard(node_id: NodeId) {
            remove combiner_state -= [ node_id =>
                let CombinerState::Reading(
                    ReaderState::Range{ start, end, cur },
                )
            ];
            have alive_bits >= [ (cur % pre.buffer_size) =>
                ((cur / pre.buffer_size) % 2 == 0)
            ];

            birds_eye let val = pre.contents.index(cur as int);
            add combiner_state += [ node_id =>
                CombinerState::Reading(
                    ReaderState::Guard{ start, end, cur, val },
                )
            ];
            assert(stored_type_inv(val, cur as int));
        }
    }

    property!{
        guard_guards(node_id: NodeId) {
            have combiner_state >= [ node_id =>
                let CombinerState::Reading(
                    ReaderState::Guard{ start, end, cur, val },
                )
            ];
            guard contents >= [ cur as int => val ];
        }
    }

    transition!{
        reader_do_unguard(node_id: NodeId) {
            remove combiner_state -= [ node_id =>
                let CombinerState::Reading(
                    ReaderState::Guard{ start, end, cur, val },
                )
            ];
            add combiner_state += [ node_id =>
                CombinerState::Reading(
                    ReaderState::Range{ start, end, cur: cur + 1 },
                )
            ];
        }
    }

    transition!{
        reader_do_finish(node_id: NodeId) {
            remove combiner_state -= [ node_id =>
                let CombinerState::Reading(
                    ReaderState::Range{ start, end, cur },
                )
            ];
            add combiner_state += [ node_id =>
                CombinerState::Idle
            ];
            remove local_heads -= [ node_id => let _ ];
            add local_heads += [ node_id => end ];
        }
    }

    transition!{
        reader_do_abort(node_id: NodeId) {
            remove combiner_state -= [ node_id => let CombinerState::Reading(r) ];
            require(r.is_Starting() || r.is_Range());
            add combiner_state += [ node_id => CombinerState::Idle ];
        }
    }





    /////// Advancing head

    transition!{
        init_advance_head(node_id: NodeId) {
            have local_heads >= [ node_id => let local_head_0 ];
            remove combiner_state -= [ node_id => CombinerState::Idle ];
            add combiner_state += [ node_id => CombinerState::AdvancingHead {
                idx: 1,
                min_head: local_head_0,
            } ];
        }
    }

    transition!{
        step_advance_head(node_id: NodeId) {
            remove combiner_state -= [ node_id =>
                let CombinerState::AdvancingHead { idx, min_head }
            ];
            require(idx < pre.num_replicas);
            have local_heads >= [ idx => let local_head_at_idx ];
            let new_min = min(min_head, local_head_at_idx);
            add combiner_state += [ node_id =>
                CombinerState::AdvancingHead { idx: idx + 1, min_head: new_min }
            ];
        }
    }

    transition!{
        abandon_advance_head(node_id: NodeId) {
            remove combiner_state -= [ node_id =>
                let CombinerState::AdvancingHead { .. }
            ];
            add combiner_state += [ node_id => CombinerState::Idle ];
        }
    }

    transition!{
        finish_advance_head(node_id: NodeId) {
            remove combiner_state -= [ node_id =>
                let CombinerState::AdvancingHead { idx, min_head }
            ];
            require(idx == pre.num_replicas);
            update head = min_head;
            add combiner_state += [ node_id => CombinerState::Idle ];
        }
    }

    /////// Advancing tail

    transition!{
        init_advance_tail(node_id: NodeId) {
            have local_heads >= [ 0 => let local_head_0 ];
            remove combiner_state -= [ node_id => CombinerState::Idle ];
            add combiner_state += [ node_id => CombinerState::AdvancingTail {
                observed_head: pre.head,
            } ];
        }
    }

    transition!{
        abandon_advance_tail(node_id: NodeId) {
            remove combiner_state -= [ node_id =>
                let CombinerState::AdvancingTail { .. }
            ];
            add combiner_state += [ node_id => CombinerState::Idle ];
        }
    }

    transition!{
        finish_advance_tail(node_id: NodeId, new_tail: nat) {
            remove combiner_state -= [ node_id =>
                let CombinerState::AdvancingTail { observed_head }
            ];

            require(pre.tail <= new_tail <= observed_head + pre.buffer_size);

            add combiner_state += [ node_id =>
                CombinerState::Appending { cur_idx: pre.tail, tail: new_tail }
            ];
            update tail = new_tail;

            birds_eye let withdrawn = Map::new(
                |i: int| pre.tail - pre.buffer_size <= i < new_tail - pre.buffer_size,
                |i: int| pre.contents.index(i),
            );

            withdraw contents -= (withdrawn)
            by {
                assert(pre.num_replicas > 0);
                assert(pre.local_heads.dom().contains(0));
                assert(observed_head <= pre.local_heads[0]);
                assert(pre.local_heads[0] <= pre.tail);
                assert(observed_head <= pre.tail);
                assert(new_tail <= pre.tail + pre.buffer_size);
                assert(new_tail - pre.buffer_size <= pre.tail);
                assert forall |i: int|
                    pre.tail - pre.buffer_size <= i < new_tail - pre.buffer_size
                    implies
                    pre.contents.dom().contains(i)
                by {
                    assert(i < pre.tail);
                    assert(pre.tail <= i + pre.buffer_size);
                    let min_local_head = map_min_value(pre.local_heads, (pre.num_replicas - 1) as nat);
                    assert(i < pre.local_heads[0]);
                    assert(i < min_local_head);
                    assert(i + pre.buffer_size < min_local_head + pre.buffer_size);
                    assert(!entry_is_alive(pre.alive_bits, i + pre.buffer_size, pre.buffer_size));
                    assert(entry_is_alive(pre.alive_bits, i, pre.buffer_size));
                }
            };

            assert(forall
              |i: int| pre.tail - pre.buffer_size <= i < new_tail - pre.buffer_size
                ==> stored_type_inv(#[trigger] withdrawn.index(i), i));
        }
    }

    transition!{
        append_flip_bit(node_id: NodeId, deposited: StoredType) {
            remove combiner_state -= [ node_id =>
                let CombinerState::Appending { cur_idx, tail }
            ];
            add combiner_state += [ node_id =>
                CombinerState::Appending { cur_idx: cur_idx + 1, tail }
            ];

            remove alive_bits -= [ (cur_idx % pre.buffer_size) => let bit ];
            //require(((cur_idx / pre.buffer_size) % 2 == 0) != bit);
            add alive_bits += [ (cur_idx % pre.buffer_size) =>
                ((cur_idx / pre.buffer_size) % 2 == 0)
            ];

            require(stored_type_inv(deposited, cur_idx as int));
            deposit contents += [ cur_idx as int => deposited ];
        }
    }

    transition!{
        finish_appending(node_id: NodeId) {
            remove combiner_state -= [ node_id =>
                let CombinerState::Appending { cur_idx, tail }
            ];

            require(cur_idx == tail);

            add combiner_state += [ node_id => CombinerState::Idle ];
        }
    }

    /////// Reading



    ///// Invariants


    /////// proofs

    #[inductive(initialize)]
    fn initialize_inductive(post: Self, buffer_size: nat, num_replicas: nat, contents: Map<int, StoredType>) { }

    #[inductive(init_advance_head)]
    fn init_advance_head_inductive(pre: Self, post: Self, node_id: NodeId) { }

    #[inductive(step_advance_head)]
    fn step_advance_head_inductive(pre: Self, post: Self, node_id: NodeId) { }

    #[inductive(abandon_advance_head)]
    fn abandon_advance_head_inductive(pre: Self, post: Self, node_id: NodeId) { }

    #[inductive(finish_advance_head)]
    fn finish_advance_head_inductive(pre: Self, post: Self, node_id: NodeId) { }

    #[inductive(init_advance_tail)]
    fn init_advance_tail_inductive(pre: Self, post: Self, node_id: NodeId) { }

    #[inductive(abandon_advance_tail)]
    fn abandon_advance_tail_inductive(pre: Self, post: Self, node_id: NodeId) { }

    #[inductive(finish_advance_tail)]
    fn finish_advance_tail_inductive(pre: Self, post: Self, node_id: NodeId, new_tail: nat) { }

    #[inductive(append_flip_bit)]
    fn append_flip_bit_inductive(pre: Self, post: Self, node_id: NodeId, deposited: StoredType) { }

    #[inductive(finish_appending)]
    fn finish_appending_inductive(pre: Self, post: Self, node_id: NodeId) { }

    #[inductive(reader_do_start)]
    fn reader_do_start_inductive(pre: Self, post: Self, node_id: NodeId) { }

    #[inductive(reader_do_enter)]
    fn reader_do_enter_inductive(pre: Self, post: Self, node_id: NodeId) { }

    #[inductive(reader_do_guard)]
    fn reader_do_guard_inductive(pre: Self, post: Self, node_id: NodeId) { }

    #[inductive(reader_do_unguard)]
    fn reader_do_unguard_inductive(pre: Self, post: Self, node_id: NodeId) { }

    #[inductive(reader_do_finish)]
    fn reader_do_finish_inductive(pre: Self, post: Self, node_id: NodeId) { }

    #[inductive(reader_do_abort)]
    fn reader_do_abort_inductive(pre: Self, post: Self, node_id: NodeId) { }
}}

verus!{
pub open spec fn min(x: nat, y: nat) -> nat {
    if x < y { x } else { y }
}

pub closed spec fn map_min_value(m: Map<NodeId, nat>, idx: nat) -> nat
  decreases idx
{
    if idx === 0 {
        m.index(0)
    } else {
        min(
            map_min_value(m, (idx - 1) as nat),
            m.index((idx - 1) as nat),
        )
    }
}

pub open spec fn entry_is_alive(alive_bits: Map</* entry: */ nat, /* bit: */ bool>, logical: int, buffer_size: nat) -> bool
{
    let phys_id = logical % buffer_size as int;
    alive_bits[phys_id as nat] == logical_to_alive_bit_alive_when(logical, buffer_size)
}

pub open spec fn logical_to_alive_bit_alive_when(logical: int, buffer_size: nat) -> bool {
    ((logical / buffer_size as int) % 2) == 0
}

}

fn main() { }