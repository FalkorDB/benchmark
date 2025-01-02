import { NextResponse } from "next/server";

export async function GET() {
  try {
    const url = "https://benchmark.falkordb.com/results-dummy.json";

    const result = await fetch(url, {
      method: "GET",
      cache: "no-store",
      headers: {
        "Content-Type": "application/json",
      },
    });

    if (!result.ok) {
      throw new Error(await result.text());
    }

    const json = await result.json();

    const response = {
      data: json,
    };

    return NextResponse.json({ result: response }, { status: 200 });
  } catch (err) {
    console.error(err);
    return NextResponse.json(
      { error: (err as Error).message || "Unknown error occurred" },
      { status: 400 }
    );
  }
}
