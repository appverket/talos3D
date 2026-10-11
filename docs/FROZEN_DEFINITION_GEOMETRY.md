# Frozen Definition geometry

A canonical `DefinitionBody` may contain one `FrozenMesh` evaluator for a
fixed evaluated shape. It participates in ordinary Definition libraries,
Occurrence placement, history, selection and persistence. It does not imply
recovered configuration logic, constraints or host contracts.

```json
{
  "schema_version": 1,
  "evaluators": [{
    "FrozenMesh": {
      "coordinate_unit": "m",
      "vertices": [[0, 0, 0], [1, 0, 0], [0, 1, 0]],
      "faces": [[0, 1, 2]]
    }
  }],
  "representations": [{
    "kind": "PrimaryGeometry",
    "role": "Body",
    "update_policy": "Frozen"
  }]
}
```

Vertices are local coordinates with an explicit recognized length unit.
World geometry uses metres; an Occurrence supplies placement and rotation.
Faces preserve source winding. The existing renderer computes flat normals.
No source format, external executable, network request or file path is part
of this evaluator. Source provenance, adapter fidelity and licenses belong to
the acquisition/domain layer and can travel in the existing opaque metadata.

The snapshot requires a Frozen representation and no geometry-affecting
parameters. Descriptive dimensions should be `Locked` metadata parameters;
changing them cannot resize the snapshot. Ordinary placement transforms still
work; transforming a shape is not a claim about available manufactured
configurations. A domain may map verified behavior to a different native body.

Validation rejects non-length units, empty/oversized arrays, nonfinite or
unrepresentable coordinates, invalid indices and degenerate triangles,
including triangles whose float32 render normals would overflow/underflow.
Limits are one million vertices and two million faces per evaluator. Array
limits bound evaluation, not an application's source-file parsing budget.
Open surfaces are permitted; no manifold/volume or fabrication claim is made.
Unknown evaluator fields are refused rather than silently discarded.

Repeated occurrences share the existing representation-cache GPU asset.
Local AABBs are available immediately for transformed selection/inspection,
including compound slots and before render extraction. Meshes remain derived;
persistence stores the canonical body and authored Occurrences, not render
components. Reimport/version updates still use the normal Definition lifecycle.
Changing the evaluator does not create a parallel public component kind.
