const url = "http://localhost:3000/api/evaluation/a13216e5-5f2f-4524-98f0-79679372c1ef/stream";
const token = "[TOKEN]";

async function makeRequest(){
    try {
        const response = await fetch(url, {
            method: "POST",
            mode: "no-cors", // TODO fix cors
            headers: {
                "Access-Control-Allow-Origin": "http://localhost:3000/",
                "Authorization": `Bearer ${token}`,
                "Content-Type": "application/json"
            },
        });

        if (!response.ok){
            throw new Error('HTTP error! Status: ${response.status}');
        }

        const data = await response.json();
        if (data) {
            console.log("Response data:", data);
        } else {
            console.log("Die API hat keine Daten zurückgegeben.");
        }
  } catch (error) {
    console.error("Error during fetch:", error);
  }
}

makeRequest();

// const request = new Request("curl -XPOST http://localhost:3000/api/build/a13216e5-5f2f-4524-98f0-79679372c1ef -H 'Authorization: Bearer [TOKEN]' -H 'Content-Type: application/json' -i");