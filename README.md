# Collision-Affording Point Trees: SIMD-Amenable Nearest Neighbors for Fast Collision Checking

This is a Rust implementation of the _collision-affording point tree_ (CAPT), a data structure for
SIMD-parallel collision-checking against point clouds.

You may also want to look at the following other sources:

- [The paper](https://arxiv.org/abs/2406.02807)
- [C++ implementation](https://github.com/KavrakiLab/vamp)
- [Blog post about it](https://www.claytonwramsey.com/blog/captree)

If you use this in an academic work, please cite it as follows:

```bibtex
@InProceedings{capt,
  title = {Collision-Affording Point Trees: {SIMD}-Amenable Nearest Neighbors for Fast Collision Checking},
  author = {Ramsey, Clayton W. and Kingston, Zachary and Tomason, Wil and Kavraki, Lydia E.},
  booktitle = {Robotics: Science and Systems},
  date = {2024},
  url = {http://arxiv.org/abs/2406.02807},
  note = {To Appear.}
}
```

## Usage

The core data structure in this library is the `Capt`, which is a search tree used for collision checking.

```rust
use captree
```

##
