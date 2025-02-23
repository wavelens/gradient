async function makeRequest() {
  try {
    fetch(url, {
      method: "POST",
      credentials: "include",
      withCredentials: true,
      mode: "cors",
      headers: {
        "Authorization": `Bearer ${token}`,
        "Content-Type": "application/jsonstream",
      },
    }).then(async (response) => {
      const reader = response.body.getReader();
      while (true) {
        const { done, value } = await reader.read();
        const text = new TextDecoder("utf-8").decode(value);
        if (done) break;
        if (text) {
          const data = JSON.parse(text);
          if (data.hasOwnProperty("error")) {
            console.error(data["message"]);
          } else {
            // Replace this with element to output
            console.log(data);
          }
        }
      }
    });
  } catch (error) {
    console.error("Error during fetch:", error);
  }
}

makeRequest();
