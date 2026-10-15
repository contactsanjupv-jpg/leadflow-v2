"use client";

import { useState, type FormEvent } from "react";
import { api } from "@/lib/api";
import { isValidDecimal } from "@/lib/money";
import DateInput from "@/components/DateInput";
import SourceDoc from "@/components/SourceDoc";
import { Card, Empty, Err, Money, useAction, useLoad } from "@/components/ui";

type Vendor = { id: string; name: string };
type Contract = {
  id: string;
  name: string;
  vendor_id: string;
  vendor_name: string;
};
type FeeDraft = {
  fee_code: string;
  description: string;
  amount: string;
};
type Rate = {
  id: string;
  waste_stream: string;
  unit: string;
  unit_rate: string;
  effective_from: string;
  effective_to: string | null;
  authorized_fees: FeeDraft[];
};
type ContractDetail = Contract & { rates?: Rate[] };
type Manifest = {
  id: string;
  manifest_number: string;
  waste_stream: string;
  quantity: string;
  unit: string;
  container_count: number | null;
  shipment_date: string;
};
type Ticket = {
  id: string;
  ticket_number: string;
  manifest_id: string | null;
  net_weight: string;
  unit: string;
  container_count: number | null;
  ticket_date: string;
};
type Tab =
  | "Vendors"
  | "Contracts & rates"
  | "Manifests"
  | "Weight tickets";

const TABS: Tab[] = [
  "Vendors",
  "Contracts & rates",
  "Manifests",
  "Weight tickets",
];

const blankFee = (): FeeDraft => ({
  fee_code: "",
  description: "",
  amount: "",
});

export default function DataManagement() {
  const [tab, setTab] = useState<Tab>("Vendors");

  const vendors = useLoad<Vendor[]>("/vendors?limit=200");
  const contracts = useLoad<Contract[]>("/contracts?limit=200");
  const manifests = useLoad<Manifest[]>("/manifests?limit=200");
  const tickets = useLoad<Ticket[]>("/weight-tickets?limit=200");

  const [selectedContractId, setSelectedContractId] = useState("");
  const selectedContract = useLoad<ContractDetail>(
    selectedContractId ? `/contracts/${selectedContractId}` : null,
  );

  const act = useAction();

  const [vendorName, setVendorName] = useState("");

  const [contractForm, setContractForm] = useState({
    vendor_id: "",
    name: "",
    source_document_id: "",
  });

  const [rateForm, setRateForm] = useState({
    waste_stream: "",
    unit: "",
    unit_rate: "",
    effective_from: "",
    effective_to: "",
  });

  const [fees, setFees] = useState<FeeDraft[]>([]);

  const [manifestForm, setManifestForm] = useState({
    manifest_number: "",
    waste_stream: "",
    container_count: "",
    quantity: "",
    unit: "",
    shipment_date: "",
    source_document_id: "",
  });

  const [ticketForm, setTicketForm] = useState({
    ticket_number: "",
    manifest_id: "",
    net_weight: "",
    unit: "",
    container_count: "",
    ticket_date: "",
    source_document_id: "",
  });

  async function createVendor(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();

    const created = await act.run(
      () => api<Vendor>("POST", "/vendors", { name: vendorName }),
      "Vendor added",
    );

    if (!created) return;

    setVendorName("");
    setContractForm((form) => ({
      ...form,
      vendor_id: created.id,
    }));

    await vendors.reload();
    await contracts.reload();
  }

  async function createContract(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();

    const created = await act.run(
      () =>
        api<ContractDetail>("POST", "/contracts", {
          vendor_id: contractForm.vendor_id,
          name: contractForm.name,
          source_document_id: contractForm.source_document_id || null,
        }),
      "Contract added",
    );

    if (!created) return;

    setSelectedContractId(created.id);

    setContractForm((form) => ({
      ...form,
      name: "",
      source_document_id: "",
    }));

    await contracts.reload();
  }

  async function createRate(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();

    if (!selectedContractId) {
      await act.run(async () => {
        throw new Error("Select a contract before adding a rate");
      });
      return;
    }

    const decimals = [
      rateForm.unit_rate,
      ...fees.map((fee) => fee.amount),
    ];

    if (decimals.some((value) => !isValidDecimal(value))) {
      await act.run(async () => {
        throw new Error(
          "Rates and authorized fees must be decimal numbers with at most 4 decimal places",
        );
      });
      return;
    }

    if (fees.some((fee) => !fee.fee_code.trim())) {
      await act.run(async () => {
        throw new Error("Every authorized fee must have a fee code");
      });
      return;
    }

    const created = await act.run(
      () =>
        api<ContractDetail>(
          "POST",
          `/contracts/${selectedContractId}/rates`,
          {
            waste_stream: rateForm.waste_stream,
            unit: rateForm.unit,
            unit_rate: rateForm.unit_rate,
            effective_from: rateForm.effective_from,
            effective_to: rateForm.effective_to || null,
            facility_id: null,
            authorized_fees: fees.map((fee) => ({
              fee_code: fee.fee_code,
              description: fee.description || null,
              amount: fee.amount,
            })),
          },
        ),
      "Rate saved",
    );

    if (!created) return;

    setRateForm({
      waste_stream: "",
      unit: "",
      unit_rate: "",
      effective_from: "",
      effective_to: "",
    });

    setFees([]);

    await selectedContract.reload();
    await contracts.reload();
  }

  async function createManifest(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();

    if (!isValidDecimal(manifestForm.quantity)) {
      await act.run(async () => {
        throw new Error(
          "Manifest quantity must be a decimal number with at most 4 decimal places",
        );
      });
      return;
    }

    const containerCount =
      manifestForm.container_count === ""
        ? null
        : Number(manifestForm.container_count);

    if (
      containerCount !== null &&
      (!Number.isInteger(containerCount) || containerCount < 0)
    ) {
      await act.run(async () => {
        throw new Error("Container count must be a non-negative whole number");
      });
      return;
    }

    const created = await act.run(
      () =>
        api<Manifest>("POST", "/manifests", {
          manifest_number: manifestForm.manifest_number,
          waste_stream: manifestForm.waste_stream,
          container_count: containerCount,
          quantity: manifestForm.quantity,
          unit: manifestForm.unit,
          shipment_date: manifestForm.shipment_date,
          source_document_id: manifestForm.source_document_id || null,
        }),
      "Manifest added",
    );

    if (!created) return;

    setManifestForm({
      manifest_number: "",
      waste_stream: "",
      container_count: "",
      quantity: "",
      unit: "",
      shipment_date: "",
      source_document_id: "",
    });

    await manifests.reload();
  }

  async function createTicket(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();

    if (!isValidDecimal(ticketForm.net_weight)) {
      await act.run(async () => {
        throw new Error(
          "Net weight must be a decimal number with at most 4 decimal places",
        );
      });
      return;
    }

    const containerCount =
      ticketForm.container_count === ""
        ? null
        : Number(ticketForm.container_count);

    if (
      containerCount !== null &&
      (!Number.isInteger(containerCount) || containerCount < 0)
    ) {
      await act.run(async () => {
        throw new Error("Container count must be a non-negative whole number");
      });
      return;
    }

    const created = await act.run(
      () =>
        api<Ticket>("POST", "/weight-tickets", {
          ticket_number: ticketForm.ticket_number,
          manifest_id: ticketForm.manifest_id || null,
          net_weight: ticketForm.net_weight,
          unit: ticketForm.unit,
          container_count: containerCount,
          ticket_date: ticketForm.ticket_date,
          source_document_id: ticketForm.source_document_id || null,
        }),
      "Weight ticket added",
    );

    if (!created) return;

    setTicketForm({
      ticket_number: "",
      manifest_id: "",
      net_weight: "",
      unit: "",
      container_count: "",
      ticket_date: "",
      source_document_id: "",
    });

    await tickets.reload();
  }

  function setFee(index: number, key: keyof FeeDraft, value: string) {
    setFees((current) =>
      current.map((fee, i) =>
        i === index ? { ...fee, [key]: value } : fee,
      ),
    );
  }

  const rateList = selectedContract.data?.rates ?? [];

  return (
    <>
      <h1>Contracts &amp; data</h1>

      <p className="muted">
        Set up the source records XFlow needs before reconciling invoices:
        vendors, contract rates, manifests, and weight tickets. Enter values
        from the source documents and link those documents for traceability.
      </p>

      <Card>
        <div className="row" aria-label="Data sections">
          {TABS.map((name) => (
            <button
              key={name}
              type="button"
              className={tab === name ? "" : "sec"}
              aria-pressed={tab === name}
              onClick={() => setTab(name)}
            >
              {name}
            </button>
          ))}
        </div>

        <Err e={act.error} />

        {act.ok && (
          <p className="okmsg" role="status">
            {act.ok}
          </p>
        )}
      </Card>

      {tab === "Vendors" && (
        <>
          <Card title="Vendors">
            <Err e={vendors.error} />

            <table>
              <thead>
                <tr>
                  <th>Vendor</th>
                  <th>Reference</th>
                </tr>
              </thead>

              <tbody>
                {(vendors.data ?? []).map((vendor) => (
                  <tr key={vendor.id}>
                    <td>{vendor.name}</td>
                    <td className="muted">{vendor.id.slice(0, 8)}</td>
                  </tr>
                ))}

                {vendors.data?.length === 0 && (
                  <tr>
                    <td colSpan={2}>
                      <Empty>
                        No vendors yet. Add the vendor shown on your invoice
                        or contract.
                      </Empty>
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </Card>

          <Card title="Add vendor">
            <form className="row" onSubmit={createVendor}>
              <label>
                Name
                <input
                  value={vendorName}
                  onChange={(e) => setVendorName(e.target.value)}
                  required
                  maxLength={200}
                />
              </label>

              <button disabled={act.busy}>Add vendor</button>
            </form>
          </Card>
        </>
      )}

      {tab === "Contracts & rates" && (
        <>
          <Card title="Contracts & rates">
            <Err e={contracts.error} />

            <table>
              <thead>
                <tr>
                  <th>Contract</th>
                  <th>Vendor</th>
                  <th>Rates</th>
                </tr>
              </thead>

              <tbody>
                {(contracts.data ?? []).map((contract) => (
                  <tr key={contract.id}>
                    <td>
                      <button
                        type="button"
                        className="sec"
                        onClick={() => setSelectedContractId(contract.id)}
                      >
                        {contract.name}
                      </button>
                    </td>

                    <td>{contract.vendor_name}</td>

                    <td>
                      <button
                        type="button"
                        className="sec"
                        onClick={() => setSelectedContractId(contract.id)}
                      >
                        View / add rates
                      </button>
                    </td>
                  </tr>
                ))}

                {contracts.data?.length === 0 && (
                  <tr>
                    <td colSpan={3}>
                      <Empty>
                        No contracts yet. Add a contract, then add its
                        effective-dated rates and authorized fees.
                      </Empty>
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </Card>

          <Card title="Add contract">
            <form onSubmit={createContract}>
              <div className="row">
                <label>
                  Vendor
                  <select
                    value={contractForm.vendor_id}
                    onChange={(e) =>
                      setContractForm({
                        ...contractForm,
                        vendor_id: e.target.value,
                      })
                    }
                    required
                  >
                    <option value="">Select vendor…</option>

                    {(vendors.data ?? []).map((vendor) => (
                      <option key={vendor.id} value={vendor.id}>
                        {vendor.name}
                      </option>
                    ))}
                  </select>
                </label>

                <label>
                  Contract name
                  <input
                    value={contractForm.name}
                    onChange={(e) =>
                      setContractForm({
                        ...contractForm,
                        name: e.target.value,
                      })
                    }
                    required
                    maxLength={200}
                  />
                </label>

                <SourceDoc
                  value={contractForm.source_document_id}
                  onChange={(value) =>
                    setContractForm({
                      ...contractForm,
                      source_document_id: value,
                    })
                  }
                />

                <button disabled={act.busy || !vendors.data?.length}>
                  Add contract
                </button>
              </div>
            </form>
          </Card>

          <Card title="Rates">
            <label>
              Contract
              <select
                value={selectedContractId}
                onChange={(e) => setSelectedContractId(e.target.value)}
              >
                <option value="">Select contract…</option>

                {(contracts.data ?? []).map((contract) => (
                  <option key={contract.id} value={contract.id}>
                    {contract.vendor_name} — {contract.name}
                  </option>
                ))}
              </select>
            </label>

            {selectedContractId ? (
              <>
                <Err e={selectedContract.error} />

                <table>
                  <thead>
                    <tr>
                      <th>Waste stream</th>
                      <th>Rate / unit</th>
                      <th>Effective from</th>
                      <th>Effective to</th>
                      <th>Authorized fees</th>
                    </tr>
                  </thead>

                  <tbody>
                    {rateList.map((rate) => (
                      <tr key={rate.id}>
                        <td>{rate.waste_stream}</td>
                        <td>
                          <Money v={rate.unit_rate} /> / {rate.unit}
                        </td>
                        <td>{rate.effective_from}</td>
                        <td>{rate.effective_to ?? "Open-ended"}</td>
                        <td>
                          {rate.authorized_fees?.length
                            ? rate.authorized_fees
                                .map(
                                  (fee) => `${fee.fee_code}: ${fee.amount}`,
                                )
                                .join("; ")
                            : "None"}
                        </td>
                      </tr>
                    ))}

                    {rateList.length === 0 && (
                      <tr>
                        <td colSpan={5}>
                          <Empty>No rates for this contract yet.</Empty>
                        </td>
                      </tr>
                    )}
                  </tbody>
                </table>

                <form onSubmit={createRate}>
                  <div className="row">
                    <label>
                      Waste stream
                      <input
                        value={rateForm.waste_stream}
                        onChange={(e) =>
                          setRateForm({
                            ...rateForm,
                            waste_stream: e.target.value,
                          })
                        }
                        required
                        maxLength={100}
                      />
                    </label>

                    <label>
                      Unit
                      <input
                        value={rateForm.unit}
                        onChange={(e) =>
                          setRateForm({
                            ...rateForm,
                            unit: e.target.value,
                          })
                        }
                        required
                        maxLength={50}
                      />
                    </label>

                    <label>
                      Unit rate
                      <input
                        value={rateForm.unit_rate}
                        onChange={(e) =>
                          setRateForm({
                            ...rateForm,
                            unit_rate: e.target.value,
                          })
                        }
                        required
                        inputMode="decimal"
                      />
                    </label>

                    <label>
                      Effective from
                      <DateInput
                        value={rateForm.effective_from}
                        onChange={(value) =>
                          setRateForm({
                            ...rateForm,
                            effective_from: value,
                          })
                        }
                        required
                      />
                    </label>

                    <label>
                      Effective to (optional)
                      <DateInput
                        value={rateForm.effective_to}
                        onChange={(value) =>
                          setRateForm({
                            ...rateForm,
                            effective_to: value,
                          })
                        }
                      />
                    </label>
                  </div>

                  <h3>Authorized fees</h3>

                  {fees.map((fee, index) => (
                    <div className="row" key={index}>
                      <label>
                        Fee code
                        <input
                          value={fee.fee_code}
                          onChange={(e) =>
                            setFee(index, "fee_code", e.target.value)
                          }
                          required
                          maxLength={50}
                        />
                      </label>

                      <label>
                        Description
                        <input
                          value={fee.description}
                          onChange={(e) =>
                            setFee(index, "description", e.target.value)
                          }
                          maxLength={200}
                        />
                      </label>

                      <label>
                        Amount
                        <input
                          value={fee.amount}
                          onChange={(e) =>
                            setFee(index, "amount", e.target.value)
                          }
                          required
                          inputMode="decimal"
                        />
                      </label>

                      <button
                        type="button"
                        className="sec"
                        aria-label={`Remove fee ${index + 1}`}
                        onClick={() =>
                          setFees((current) =>
                            current.filter((_, i) => i !== index),
                          )
                        }
                      >
                        Remove
                      </button>
                    </div>
                  ))}

                  <div className="row">
                    <button
                      type="button"
                      className="sec"
                      onClick={() =>
                        setFees((current) => [...current, blankFee()])
                      }
                    >
                      Add authorized fee
                    </button>

                    <button disabled={act.busy}>Save rate</button>
                  </div>
                </form>
              </>
            ) : (
              <Empty>
                Select an existing contract or create one above to add rates.
              </Empty>
            )}
          </Card>
        </>
      )}

      {tab === "Manifests" && (
        <Card title="Manifests">
          <Err e={manifests.error} />

          <table>
            <thead>
              <tr>
                <th>Manifest #</th>
                <th>Waste stream</th>
                <th>Quantity</th>
                <th>Containers</th>
                <th>Shipment date</th>
              </tr>
            </thead>

            <tbody>
              {(manifests.data ?? []).map((manifest) => (
                <tr key={manifest.id}>
                  <td>{manifest.manifest_number}</td>
                  <td>{manifest.waste_stream}</td>
                  <td>
                    {manifest.quantity} {manifest.unit}
                  </td>
                  <td>{manifest.container_count ?? "—"}</td>
                  <td>{manifest.shipment_date}</td>
                </tr>
              ))}

              {manifests.data?.length === 0 && (
                <tr>
                  <td colSpan={5}>
                    <Empty>
                      No manifests yet. Add the shipment manifest used to
                      support an invoice.
                    </Empty>
                  </td>
                </tr>
              )}
            </tbody>
          </table>

          <form onSubmit={createManifest}>
            <div className="row">
              <label>
                Manifest #
                <input
                  value={manifestForm.manifest_number}
                  onChange={(e) =>
                    setManifestForm({
                      ...manifestForm,
                      manifest_number: e.target.value,
                    })
                  }
                  required
                  maxLength={100}
                />
              </label>

              <label>
                Waste stream
                <input
                  value={manifestForm.waste_stream}
                  onChange={(e) =>
                    setManifestForm({
                      ...manifestForm,
                      waste_stream: e.target.value,
                    })
                  }
                  required
                  maxLength={100}
                />
              </label>

              <label>
                Containers
                <input
                  value={manifestForm.container_count}
                  onChange={(e) =>
                    setManifestForm({
                      ...manifestForm,
                      container_count: e.target.value,
                    })
                  }
                  type="number"
                  min="0"
                  step="1"
                />
              </label>

              <label>
                Quantity
                <input
                  value={manifestForm.quantity}
                  onChange={(e) =>
                    setManifestForm({
                      ...manifestForm,
                      quantity: e.target.value,
                    })
                  }
                  required
                  inputMode="decimal"
                />
              </label>

              <label>
                Unit
                <input
                  value={manifestForm.unit}
                  onChange={(e) =>
                    setManifestForm({
                      ...manifestForm,
                      unit: e.target.value,
                    })
                  }
                  required
                  maxLength={50}
                />
              </label>

              <label>
                Shipment date
                <DateInput
                  value={manifestForm.shipment_date}
                  onChange={(value) =>
                    setManifestForm({
                      ...manifestForm,
                      shipment_date: value,
                    })
                  }
                  required
                />
              </label>

              <SourceDoc
                value={manifestForm.source_document_id}
                onChange={(value) =>
                  setManifestForm({
                    ...manifestForm,
                    source_document_id: value,
                  })
                }
              />

              <button disabled={act.busy}>Add manifest</button>
            </div>
          </form>
        </Card>
      )}

      {tab === "Weight tickets" && (
        <Card title="Weight tickets">
          <Err e={tickets.error} />

          <table>
            <thead>
              <tr>
                <th>Ticket #</th>
                <th>Manifest</th>
                <th>Net weight</th>
                <th>Containers</th>
                <th>Ticket date</th>
              </tr>
            </thead>

            <tbody>
              {(tickets.data ?? []).map((ticket) => (
                <tr key={ticket.id}>
                  <td>{ticket.ticket_number}</td>
                  <td>
                    {manifests.data?.find(
                      (manifest) => manifest.id === ticket.manifest_id,
                    )?.manifest_number ?? "—"}
                  </td>
                  <td>
                    {ticket.net_weight} {ticket.unit}
                  </td>
                  <td>{ticket.container_count ?? "—"}</td>
                  <td>{ticket.ticket_date}</td>
                </tr>
              ))}

              {tickets.data?.length === 0 && (
                <tr>
                  <td colSpan={5}>
                    <Empty>
                      No weight tickets yet. Link a ticket to the manifest
                      when available.
                    </Empty>
                  </td>
                </tr>
              )}
            </tbody>
          </table>

          <form onSubmit={createTicket}>
            <div className="row">
              <label>
                Ticket #
                <input
                  value={ticketForm.ticket_number}
                  onChange={(e) =>
                    setTicketForm({
                      ...ticketForm,
                      ticket_number: e.target.value,
                    })
                  }
                  required
                  maxLength={100}
                />
              </label>

              <label>
                Manifest
                <select
                  value={ticketForm.manifest_id}
                  onChange={(e) =>
                    setTicketForm({
                      ...ticketForm,
                      manifest_id: e.target.value,
                    })
                  }
                >
                  <option value="">None</option>

                  {(manifests.data ?? []).map((manifest) => (
                    <option key={manifest.id} value={manifest.id}>
                      {manifest.manifest_number}
                    </option>
                  ))}
                </select>
              </label>

              <label>
                Net weight
                <input
                  value={ticketForm.net_weight}
                  onChange={(e) =>
                    setTicketForm({
                      ...ticketForm,
                      net_weight: e.target.value,
                    })
                  }
                  required
                  inputMode="decimal"
                />
              </label>

              <label>
                Unit
                <input
                  value={ticketForm.unit}
                  onChange={(e) =>
                    setTicketForm({
                      ...ticketForm,
                      unit: e.target.value,
                    })
                  }
                  required
                  maxLength={50}
                />
              </label>

              <label>
                Containers
                <input
                  value={ticketForm.container_count}
                  onChange={(e) =>
                    setTicketForm({
                      ...ticketForm,
                      container_count: e.target.value,
                    })
                  }
                  type="number"
                  min="0"
                  step="1"
                />
              </label>

              <label>
                Ticket date
                <DateInput
                  value={ticketForm.ticket_date}
                  onChange={(value) =>
                    setTicketForm({
                      ...ticketForm,
                      ticket_date: value,
                    })
                  }
                  required
                />
              </label>

              <SourceDoc
                value={ticketForm.source_document_id}
                onChange={(value) =>
                  setTicketForm({
                    ...ticketForm,
                    source_document_id: value,
                  })
                }
              />

              <button disabled={act.busy}>Add ticket</button>
            </div>
          </form>
        </Card>
      )}
    </>
  );
}