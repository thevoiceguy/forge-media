#!/usr/bin/env python3
"""Specialize silero_vad_op18_ifless.onnx into per-sample-rate models.

The upstream model wraps two complete per-rate subgraphs (16 kHz / 8 kHz)
in a top-level ONNX `If` node switched on the `sr` input. tract-onnx
rejects that `If` during type inference, so we extract the branch for
each rate into a standalone graph: same nodes, same weights, no control
flow, no `sr` input.

Usage: specialize.py <ifless.onnx> <out_16k.onnx> <out_8k.onnx>
"""
import sys

import onnx
from onnx import helper

CONTEXT = {"16k": 64, "8k": 32}
WINDOW = {"16k": 512, "8k": 256}


def specialize(model, branch_attr, tag):
    g = model.graph
    if_node = next(n for n in g.node if n.op_type == "If")
    branch = next(
        helper.get_attribute_value(a)
        for a in if_node.attribute
        if a.name == branch_attr
    )

    # Map branch subgraph outputs to the parent If node's outputs.
    rename = {
        sub_out.name: parent_out
        for sub_out, parent_out in zip(branch.output, if_node.output)
    }

    nodes = []
    for n in branch.node:
        n2 = onnx.NodeProto()
        n2.CopyFrom(n)
        del n2.output[:]
        n2.output.extend(rename.get(o, o) for o in n.output)
        del n2.input[:]
        n2.input.extend(rename.get(i, i) for i in n.input)
        nodes.append(n2)

    # Keep only initializers the branch actually references.
    referenced = {i for n in nodes for i in n.input}
    initializers = [i for i in g.initializer if i.name in referenced]

    input_len = WINDOW[tag] + CONTEXT[tag]
    inputs = [
        helper.make_tensor_value_info("input", onnx.TensorProto.FLOAT, [1, input_len]),
        helper.make_tensor_value_info("state", onnx.TensorProto.FLOAT, [2, 1, 128]),
    ]
    outputs = [
        helper.make_tensor_value_info("output", onnx.TensorProto.FLOAT, [1, 1]),
        helper.make_tensor_value_info("stateN", onnx.TensorProto.FLOAT, [2, 1, 128]),
    ]

    new_graph = helper.make_graph(
        nodes, f"silero_vad_{tag}", inputs, outputs, initializers
    )
    new_model = helper.make_model(
        new_graph,
        opset_imports=list(model.opset_import),
        ir_version=model.ir_version,
        producer_name="forge-media specialize.py",
    )
    onnx.checker.check_model(new_model)
    return new_model


def main():
    src, out16, out8 = sys.argv[1], sys.argv[2], sys.argv[3]
    model = onnx.load(src)
    onnx.save(specialize(model, "then_branch", "16k"), out16)
    onnx.save(specialize(model, "else_branch", "8k"), out8)
    print(f"wrote {out16} and {out8}")


if __name__ == "__main__":
    main()
