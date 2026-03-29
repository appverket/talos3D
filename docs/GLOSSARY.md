# Glossary

## AI-first

The principle that AI is a primary consumer of the platform, not an afterthought
integration.

## Authored Model

The persistent source of truth: entities, definition relationships, semantic
parameters, metadata, and invariants.

## Authored Solid Envelope (ASE)

An AI-facing semantic summary for a realized solid. It describes definition
role, topology intent, inputs, invariants, attached features, and evaluated body
facts.

## Capability

The primary extension unit. A capability can contribute authored entities,
commands, tools, panels, formats, rules, and AI-visible semantics.

## Definition Graph

The authored dependency structure relating geometry definitions, features, and
domain entities. This should stay compatible with trees and DAGs.

## Evaluated Body

The realized geometric body derived from authored definitions before final mesh
generation. This is where connectedness, volume, and manifold status are
computed.

## Generated Face Reference

A stable semantic face identifier exposed above raw topology indices.

## Semantic Affordance Surface (SAS)

The principle that editing affordances should come from authored semantics and
invariants rather than inferred rendered topology.

## Setup

A curated bundle of capabilities plus workflow/UI defaults.

## Topology Intent

The authored claim about what kind of body a definition is intended to produce,
for example one closed solid, a composite assembly, or an open surface.
